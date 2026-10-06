//! Transfer and temporal author curve editor primitive.
//!
//! This widget owns screen interaction; temporal values retain f64 author units.
//! Domain layers map keyframes, effect curves, or tone curves into this compact
//! representation and commit mutations outside the widget.

mod model;
mod rulers;

use model::{constrained_point, normalize_points, CurveGeometry, CurvePointSpacing, CurveViewport};
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, AccessibilityValue, EventContext,
    PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::Theme;

use crate::context_menu::ContextMenu;
use crate::menu::MenuItem;
use crate::paint::{color_with_alpha, paint_focus_ring};
use crate::ValueEdit;

const DEFAULT_WIDTH: f32 = 220.0;
const DEFAULT_HEIGHT: f32 = 104.0;
const PADDING: f32 = 10.0;
const HIT_RADIUS: f32 = 8.0;
const POINT_RADIUS: f32 = 4.0;
const SELECTED_POINT_RADIUS: f32 = 5.5;
const MIN_POINT_GAP: f32 = 0.001;

#[derive(Clone, Copy, Debug)]
struct CurveEditorVisualTokens {
    pub grid_alpha: f32,
    pub curve_alpha: f32,
    pub plot_surface_alpha: f32,
    pub point_border_alpha: f32,
    pub grid_line_width: f32,
    pub curve_line_width: f32,
    pub point_border_inset: f32,
    pub selected_inner_inset: f32,
}

impl CurveEditorVisualTokens {
    fn from_theme(_theme: &Theme) -> Self {
        Self {
            grid_alpha: 0.38,
            curve_alpha: 0.82,
            plot_surface_alpha: 0.34,
            point_border_alpha: 0.70,
            grid_line_width: 1.0,
            curve_line_width: 2.0,
            point_border_inset: -1.0,
            selected_inner_inset: 1.5,
        }
    }
}

/// A normalized editable curve point.
///
/// [`Self::new`] clamps key coordinates to the inclusive `0.0..=1.0` range.
/// Canonical tangent projections may carry finite values outside this viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurvePoint {
    /// Normalized horizontal position.
    pub x: f32,
    /// Absolute author value for temporal curves, or normalized transfer value.
    pub y: f64,
}

impl CurvePoint {
    /// Create a bounded transfer point at its canonical f32 storage precision.
    pub fn new(x: f32, y: f64) -> Self {
        Self {
            x: x.clamp(0.0, 1.0),
            y: f64::from(y.clamp(0.0, 1.0) as f32),
        }
    }
}

/// Validated value coordinates for a temporal author curve.
/// Soft bounds initialize the view; hard bounds constrain key input, never controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurveValueAxis {
    soft: mondrian_core::automation::ParameterNumericRange,
    hard: mondrian_core::automation::ParameterNumericRange,
    step: Option<f64>,
}

impl CurveValueAxis {
    /// Validate author bounds and an optional keyboard step in author units.
    pub fn new(
        soft: mondrian_core::automation::ParameterNumericRange,
        hard: mondrian_core::automation::ParameterNumericRange,
        step: Option<f64>,
    ) -> Result<Self, mondrian_core::automation::ParameterSchemaError> {
        mondrian_core::automation::ParameterNumericRange::new(soft.min, soft.max)?;
        mondrian_core::automation::ParameterNumericRange::new(hard.min, hard.max)?;
        mondrian_core::automation::ParameterNumericContract::new(
            hard,
            soft,
            step,
            mondrian_core::automation::ParameterInvalidValuePolicy::Reject,
        )?;
        Ok(Self { soft, hard, step })
    }

    fn initial_range(self) -> mondrian_core::automation::ParameterNumericRange {
        if self.soft.min == self.soft.max && self.hard.min < self.hard.max {
            self.hard
        } else {
            self.soft
        }
    }

    /// Initial visible interval in absolute author units.
    pub fn soft_range(self) -> mondrian_core::automation::ParameterNumericRange {
        self.soft
    }

    /// Admitted key values in absolute author units.
    pub fn hard_range(self) -> mondrian_core::automation::ParameterNumericRange {
        self.hard
    }
}

/// Interaction policy for one editable curve point.
///
/// This separates domain-owned points from viewport anchors. For example, an
/// animation key at the start of a Clip remains movable and deletable, while a
/// virtual point used only to display the Clip boundary can stay fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurvePointPolicy {
    /// Keep the point's horizontal position fixed during pointer and keyboard
    /// edits.
    pub lock_x: bool,
    /// Keep the author value fixed during pointer and keyboard edits.
    pub lock_y: bool,
    /// Allow Delete or Backspace to remove this point.
    pub deletable: bool,
}

impl CurvePointPolicy {
    /// Policy for a virtual viewport anchor.
    pub const fn anchor() -> Self {
        Self { lock_x: true, lock_y: false, deletable: false }
    }

    /// Policy for a domain-owned editable point.
    pub const fn editable() -> Self {
        Self { lock_x: false, lock_y: false, deletable: true }
    }
}

/// Adapter that maps the current curve points to an editor [`Action`].
pub type CurveChangeAction = dyn Fn(&[CurvePoint]) -> Option<Action>;

/// Adapter that builds commands for a complete stable selection when its menu opens.
pub type CurveSelectionMenu = dyn Fn(&[mondrian_core::KeyframeId]) -> Vec<MenuItem>;

/// One canonical tangent projected into the curve viewport.
/// Value coordinates may exceed the visible interval; they are never clamped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurveTangent {
    /// Stable author identity of the owning keyframe.
    pub keyframe_id: mondrian_core::KeyframeId,
    /// Canonical temporal side.
    pub side: mondrian_core::BezierHandleSide,
    /// Viewport coordinates of the actual control point.
    pub point: CurvePoint,
    /// Inclusive horizontal limits admitted by the neighboring author span.
    pub time_limits: [f32; 2],
}

/// Canonical curve samples and tangent positions after one author intent.
#[derive(Debug, Clone, PartialEq)]
pub struct CurvePreview {
    /// Evaluated samples for curve painting.
    pub display_points: Vec<CurvePoint>,
    /// Complete current tangent projection, including constrained partners.
    pub tangents: Vec<CurveTangent>,
    /// Canonical positions of authored keys in owner-time order, bound by identity.
    /// Order distinguishes exact times that share one projected coordinate.
    pub key_points: Vec<(mondrian_core::KeyframeId, CurvePoint)>,
    /// Evaluated owner boundaries; absent where an authored key owns that exact time.
    pub virtual_boundaries: [Option<CurvePoint>; 2],
}

/// An exact endpoint of the visible owner interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurveBoundary {
    /// Start of the owner interval.
    Start,
    /// End of the owner interval.
    End,
}

/// Stable source of one editable point, independent of its current display slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurvePointTarget {
    /// A keyframe in the owner's canonical author state.
    Keyframe(mondrian_core::KeyframeId),
    /// A point in the immutable normalized-value snapshot supplied by the owner.
    Snapshot(usize),
    /// An evaluated temporal viewport boundary with no authored keyframe yet.
    VirtualBoundary(CurveBoundary),
}

/// Point, selection or tangent author intent, carrying stable source identities.
#[derive(Debug, Clone, PartialEq)]
pub enum CurveEdit {
    /// Offset an authored scalar key using its canonical typed value.
    NudgeValue {
        /// Stable owning key identity.
        keyframe_id: mondrian_core::KeyframeId,
        /// Author-unit step, independent of projected key geometry.
        delta: f64,
        /// Signed number of steps, retaining coarse increments without overflow.
        steps: i8,
    },
    /// Offset a complete selected key set with one common bounded increment.
    NudgeValueMany {
        /// Stable identities in current author order.
        keyframe_ids: Vec<mondrian_core::KeyframeId>,
        /// Author-unit increment, independent of projected geometry.
        delta: f64,
        /// Signed increment count.
        steps: i8,
    },
    /// Adjust an authored key or control by an exact owner-time increment.
    NudgeTime {
        /// Stable owning key identity.
        keyframe_id: mondrian_core::KeyframeId,
        /// Control side, or `None` for the key itself.
        side: Option<mondrian_core::BezierHandleSide>,
        /// Signed exact owner-time delta derived from an explicit UI frame grid.
        delta: mondrian_core::TimelineTime,
    },
    /// Translate the admitted authored-key selection by one exact owner-time delta.
    NudgeTimeMany {
        /// Stable identities in current author order.
        keyframe_ids: Vec<mondrian_core::KeyframeId>,
        /// Signed exact owner-time translation.
        delta: mondrian_core::TimelineTime,
    },
    /// Insert a new point between existing neighbors.
    Insert {
        /// Identity reserved for every update of this inserted point.
        keyframe_id: mondrian_core::KeyframeId,
        /// Current display slot, never an author identity.
        index: usize,
        /// Normalized owner time and absolute temporal value (or bounded transfer value).
        point: CurvePoint,
    },
    /// Move an existing point without changing its identity.
    Move {
        /// Stable author or owner-snapshot target.
        target: CurvePointTarget,
        /// Normalized owner time and absolute temporal value (or bounded transfer value).
        point: CurvePoint,
    },
    /// Move one Bezier control without changing its owner or temporal side.
    Handle {
        /// Stable author identity of the owning keyframe.
        keyframe_id: mondrian_core::KeyframeId,
        /// Canonical temporal side of the control.
        side: mondrian_core::BezierHandleSide,
        /// Finite viewport coordinates, with unconstrained value excursions.
        point: CurvePoint,
    },
    /// Delete an admitted set of authored keys as one transaction.
    DeleteMany {
        /// Stable identities in current author order; virtual boundaries are excluded.
        keyframe_ids: Vec<mondrian_core::KeyframeId>,
    },
    /// Delete one existing point.
    Delete {
        /// Stable author or owner-snapshot target.
        target: CurvePointTarget,
    },
}

/// Adapter that maps one committed point edit to an editor [`Action`].
pub type CurveEditAction = dyn Fn(CurveEdit) -> Option<Action>;

/// Lifecycle of a pointer curve gesture with live author updates and one history entry.
#[derive(Debug, Clone, PartialEq)]
pub enum CurveGestureEvent {
    /// Open a pointer draft without committing author state.
    Begin(mondrian_core::AuthoringGestureId),
    /// Apply the current coordinates; keyboard edits have no pointer gesture identity.
    Edit {
        gesture_id: Option<mondrian_core::AuthoringGestureId>,
        edit: CurveEdit,
    },
    /// Accept the pointer draft after its optional edit.
    Finish(mondrian_core::AuthoringGestureId),
    /// Restore the pointer gesture's original author state.
    Cancel(mondrian_core::AuthoringGestureId),
}

/// Adapter for explicit local curve draft ownership and committed edits.
pub type CurveGestureAction = dyn Fn(CurveGestureEvent) -> Option<Action>;

/// Project gesture lifecycle and edits using the owner's canonical evaluator.
/// Only `Edit` results replace display samples; boundaries maintain projection state.
/// A missing `Edit` projection rejects its local candidate and author dispatch.
pub type CurvePreviewProjection = dyn FnMut(CurveGestureEvent) -> Option<CurvePreview>;

/// Adapter for complete live curve values and their authoring boundaries.
pub type CurveValueEditAction = dyn Fn(ValueEdit, &[CurvePoint]) -> Option<Action>;

#[derive(Debug, Clone, Copy, PartialEq)]
struct CurvePointDrag {
    index: usize,
    origin_index: usize,
    origin: CurvePoint,
    pointer_origin: Point,
    inserted: bool,
    target_origin: CurvePointTarget,
    keyframe_id: mondrian_core::KeyframeId,
}

#[derive(Debug, Clone, Copy)]
struct ViewportDrag {
    origin: CurveViewport,
    pointer_origin: Point,
}

#[derive(Debug, Clone, Copy)]
struct TangentDrag {
    tangent: CurveTangent,
    pointer_origin: Point,
}

#[derive(Clone)]
struct CurvePointMetadata {
    target: CurvePointTarget,
    policy: CurvePointPolicy,
    context_menu: Option<Vec<MenuItem>>,
}

type CurveBoundaryOrigin = [Option<(usize, CurvePoint, CurvePointMetadata)>; 2];

/// Inactive curve selection and focus, scoped to an explicit owner view key.
/// This snapshot contains no pointer capture, author draft or callbacks.
#[derive(Debug, Clone)]
pub struct CurveEditorViewState {
    viewport: CurveViewport,
    view_key: String,
    id: WidgetId,
    points: Option<Vec<CurvePoint>>,
    selected: Option<CurvePointTarget>,
    selected_keys: std::collections::HashSet<mondrian_core::KeyframeId>,
    selected_tangent: Option<(mondrian_core::KeyframeId, mondrian_core::BezierHandleSide)>,
    focused: bool,
    focus_visible: bool,
}

impl CurveEditorViewState {
    /// Owner key used by a shell to match a replacement view.
    pub fn view_key(&self) -> &str {
        &self.view_key
    }
}

/// Interactive bounded transfer or absolute temporal author curve editor.
pub struct CurveEditor {
    id: WidgetId,
    view_key: Option<String>,
    accessible_name: Option<String>,
    keyboard_help: Option<String>,
    frame_time_step: Option<mondrian_core::TimelineTime>,
    time_axis: Option<mondrian_core::TimelineTimeRange>,
    ruler_insets: (f32, f32),
    bounds: Rect,
    viewport: CurveViewport,
    panning: Option<ViewportDrag>,
    points: Vec<CurvePoint>,
    fixed_value: Option<f64>,
    value_axis: Option<CurveValueAxis>,
    point_insertion_limit: Option<usize>,
    point_metadata: Vec<CurvePointMetadata>,
    keyframe_indices: std::collections::HashMap<mondrian_core::KeyframeId, usize>,
    display_points: Option<Vec<CurvePoint>>,
    preview_projection: Option<Box<CurvePreviewProjection>>,
    drag_display_origin: Option<Option<Vec<CurvePoint>>>,
    selected: Option<usize>,
    selected_keys: std::collections::HashSet<mondrian_core::KeyframeId>,
    selection_deletion: bool,
    selection_time_movement: bool,
    selection_value_movement: bool,
    tangents: Vec<CurveTangent>,
    selected_tangent: Option<(mondrian_core::KeyframeId, mondrian_core::BezierHandleSide)>,
    tangent_drag: Option<TangentDrag>,
    tangent_origin: Option<Vec<CurveTangent>>,
    dragging: Option<CurvePointDrag>,
    drag_value_origin: Option<(Vec<CurvePoint>, Vec<CurvePointMetadata>)>,
    drag_boundary_origin: Option<CurveBoundaryOrigin>,
    grid_columns: usize,
    grid_rows: usize,
    focused: bool,
    focus_visible: bool,
    pending_capture_release: bool,
    enabled: bool,
    on_change: Option<Box<CurveChangeAction>>,
    on_edit: Option<Box<CurveEditAction>>,
    on_gesture: Option<Box<CurveGestureAction>>,
    on_value_edit: Option<Box<CurveValueEditAction>>,
    gesture: Option<mondrian_core::AuthoringGestureId>,
    selection_menu: Option<Box<CurveSelectionMenu>>,
    context_menu: Option<ContextMenu>,
}

impl CurveEditor {
    /// Create a curve editor with a simple ease-like default curve.
    pub fn new() -> Self {
        Self::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.35, 0.62),
            CurvePoint::new(0.7, 0.44),
            CurvePoint::new(1.0, 1.0),
        ])
    }

    /// Create a curve editor from normalized points.
    pub fn with_points(points: Vec<CurvePoint>) -> Self {
        Self::from_points(points, None)
    }

    /// Show owner-domain seconds and absolute value rulers for a temporal curve.
    /// The interval is a display projection; authoring remains in the domain adapter.
    pub fn with_time_axis(mut self, range: mondrian_core::TimelineTimeRange) -> Self {
        self.time_axis = Some(range);
        self
    }

    /// Plot bounds after reserving ruler space, in widget screen coordinates.
    pub fn plot_bounds(&self) -> Rect {
        self.plot_rect()
    }

    /// Project a displayed curve coordinate into the current screen viewport.
    pub fn point_position(&self, point: CurvePoint) -> Point {
        self.to_screen(point)
    }

    /// Create a temporal curve with absolute f64 author values and a soft initial view.
    pub fn with_author_points(points: Vec<CurvePoint>, axis: CurveValueAxis) -> Self {
        Self::from_points(points, Some(axis))
    }

    fn from_points(points: Vec<CurvePoint>, value_axis: Option<CurveValueAxis>) -> Self {
        let mut editor = Self {
            id: WidgetId::new(),
            view_key: None,
            accessible_name: None,
            keyboard_help: None,
            frame_time_step: None,
            time_axis: None,
            ruler_insets: (0.0, 0.0),
            bounds: Rect::ZERO,
            viewport: value_axis.map_or_else(CurveViewport::default, |axis| {
                CurveViewport::with_value_range(axis.initial_range())
            }),
            panning: None,
            points: Vec::new(),
            fixed_value: value_axis
                .filter(|axis| axis.hard.min == axis.hard.max)
                .map(|axis| axis.hard.min),
            value_axis,
            point_insertion_limit: None,
            point_metadata: Vec::new(),
            keyframe_indices: std::collections::HashMap::new(),
            display_points: None,
            preview_projection: None,
            drag_display_origin: None,
            selected: None,
            selected_keys: std::collections::HashSet::new(),
            selection_deletion: false,
            selection_time_movement: false,
            selection_value_movement: false,
            tangents: Vec::new(),
            selected_tangent: None,
            tangent_drag: None,
            tangent_origin: None,
            dragging: None,
            drag_value_origin: None,
            drag_boundary_origin: None,
            grid_columns: 4,
            grid_rows: 3,
            focused: false,
            focus_visible: false,
            pending_capture_release: false,
            enabled: true,
            on_change: None,
            on_edit: None,
            on_gesture: None,
            on_value_edit: None,
            gesture: None,
            selection_menu: None,
            context_menu: None,
        };
        editor.set_points(points);
        editor
    }

    /// Limit the total point count admitted by user insertion.
    ///
    /// Existing and programmatically supplied points are retained. Moving and
    /// deleting points remain available at the limit. Zero prevents insertion;
    /// the default editor has no insertion limit.
    pub fn with_point_insertion_limit(mut self, maximum: usize) -> Self {
        self.point_insertion_limit = Some(maximum);
        self
    }

    /// Fix all point values to one finite coordinate admitted by this value axis.
    /// Time movement, insertion, deletion and interpolation remain available.
    /// Invalid coordinates are ignored. This also constrains supplied points
    /// and samples, regardless of builder order or later point replacement.
    pub fn with_fixed_value(mut self, value: f64) -> Self {
        if !value.is_finite()
            || self.value_axis.map_or(!(0.0..=1.0).contains(&value), |axis| {
                value < axis.hard.min || value > axis.hard.max
            })
        {
            return self;
        }
        let value = if self.value_axis.is_none() {
            f64::from(value as f32)
        } else {
            value
        };
        self.fixed_value = Some(value);
        Self::constrain_values(&mut self.points, self.fixed_value);
        if let Some(points) = &mut self.display_points {
            Self::constrain_values(points, self.fixed_value);
        }
        self
    }

    fn constrain_values(points: &mut [CurvePoint], value: Option<f64>) {
        if let Some(value) = value {
            for point in points {
                point.y = value;
            }
        }
    }

    fn constrained_value(&self, mut point: CurvePoint) -> CurvePoint {
        let bounds = self.value_axis.map_or([0.0, 1.0], |axis| [axis.hard.min, axis.hard.max]);
        point.y = point.y.clamp(bounds[0], bounds[1]);
        if self.value_axis.is_none() {
            point.y = f64::from(point.y as f32);
        }
        if let Some(value) = self.fixed_value {
            point.y = value;
        }
        point
    }

    /// Override the interaction policy for each point.
    ///
    /// Policies must have the same length and order as [`Self::points`].
    /// A mismatched policy list is ignored, preserving the safe default of
    /// fixed, non-deletable endpoints and editable interior points.
    pub fn with_point_policies(mut self, policies: Vec<CurvePointPolicy>) -> Self {
        self.set_point_policies(policies);
        self
    }

    /// Build single-key or group commands only when an authored selection menu opens.
    /// Right-clicking a selected key retains the group; Shift+F10 uses the same menu.
    pub fn with_selection_context_menu(
        mut self,
        menu: impl Fn(&[mondrian_core::KeyframeId]) -> Vec<MenuItem> + 'static,
    ) -> Self {
        self.selection_menu = Some(Box::new(menu));
        self
    }

    fn open_selection_menu(&mut self, position: Point, keyboard: bool, ctx: &mut EventContext) {
        if self.has_active_pointer_interaction() || self.gesture.is_some() {
            return;
        }
        let Some(factory) = &self.selection_menu else {
            return;
        };
        let items = factory(&self.selected_keyframe_ids());
        if !items.iter().any(MenuItem::is_activatable) {
            return;
        }
        let mut menu = ContextMenu::new(position, items);
        menu.layout(self.bounds);
        if keyboard {
            menu.event(
                &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
                ctx,
            );
        }
        self.context_menu = Some(menu);
        ctx.tooltip.hide();
        ctx.request_repaint();
    }

    /// Set optional context menu commands for each editable point.
    /// A mismatched list is ignored, and points without commands use `None`.
    pub fn with_point_context_menus(mut self, menus: Vec<Option<Vec<MenuItem>>>) -> Self {
        if menus.len() == self.points.len() {
            for (metadata, menu) in self.point_metadata.iter_mut().zip(menus) {
                metadata.context_menu = menu;
            }
        }
        self
    }

    /// Replace the interaction policy for each point.
    ///
    /// See [`Self::with_point_policies`] for mismatch behavior.
    pub fn set_point_policies(&mut self, policies: Vec<CurvePointPolicy>) {
        if policies.len() == self.points.len() {
            for (metadata, policy) in self.point_metadata.iter_mut().zip(policies) {
                metadata.policy = policy;
            }
        }
    }

    /// Dispatch a value-aware action whenever user input changes curve points.
    pub fn on_change<F, R>(mut self, action: F) -> Self
    where
        F: Fn(&[CurvePoint]) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_gesture = None;
        self.on_value_edit = None;
        self.on_change = Some(Box::new(move |points| action(points).into()));
        self
    }

    /// Dispatch one incremental edit after a pointer gesture commits.
    ///
    /// Unlike [`Self::on_change`], pointer drags emit exactly once on release.
    /// Keyboard nudges and deletion are already atomic gestures and emit
    /// immediately.
    pub fn on_edit<F, R>(mut self, action: F) -> Self
    where
        F: Fn(CurveEdit) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_gesture = None;
        self.on_value_edit = None;
        self.on_edit = Some(Box::new(move |edit| action(edit).into()));
        self
    }

    /// Dispatch pointer lifecycle boundaries and atomic edits instead of `on_edit`.
    /// Pointer drafts begin before release and emit Finish or Cancel once; keyboard
    /// edits carry no pointer identity. This replaces both value and incremental
    /// edit callbacks. Current coordinates reach the author during capture.
    pub fn on_gesture<F, R>(mut self, action: F) -> Self
    where
        F: Fn(CurveGestureEvent) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_edit = None;
        self.on_change = None;
        self.on_value_edit = None;
        self.on_gesture = Some(Box::new(move |event| action(event).into()));
        self
    }

    /// Paint samples derived by the owner's canonical curve evaluator during edits.
    /// Lifecycle events let the projection restore its derived snapshot on cancel.
    /// The widget also restores samples present before the pointer gesture began.
    /// Only edit events can replace the sampled display. A missing edit projection
    /// rejects the candidate; absent lifecycle projections still admit boundaries.
    pub fn with_preview_projection(
        mut self,
        project: impl FnMut(CurveGestureEvent) -> Option<CurvePreview> + 'static,
    ) -> Self {
        self.preview_projection = Some(Box::new(project));
        self
    }

    /// Dispatch complete live values inside one explicit pointer edit.
    /// Begin precedes insertion or movement; each update retains its identity.
    /// Release and focus loss accept, while Escape or disabling cancels. Keyboard
    /// changes without capture are independent edits. Replaces other callbacks.
    pub fn on_value_edit<F, R>(mut self, action: F) -> Self
    where
        F: Fn(ValueEdit, &[CurvePoint]) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_change = None;
        self.on_edit = None;
        self.on_gesture = None;
        self.on_value_edit = Some(Box::new(move |phase, points| action(phase, points).into()));
        self
    }

    /// Supply a read-only sampled curve used for painting.
    ///
    /// Editable points remain the hit targets. This lets a domain Adapter show
    /// Hold or Bezier evaluation without teaching the generic widget those
    /// interpolation semantics.
    pub fn with_display_points(mut self, points: Vec<CurvePoint>) -> Self {
        self.set_display_points(points);
        self
    }

    /// Replace the read-only sampled curve used for painting.
    pub fn set_display_points(&mut self, points: Vec<CurvePoint>) {
        let mut points = normalize_points(points, self.value_axis.is_some());
        Self::constrain_values(&mut points, self.fixed_value);
        self.display_points = Some(points);
    }

    /// Paint through the editable points again.
    pub fn clear_display_points(&mut self) {
        self.display_points = None;
    }

    /// Set whether the editor accepts pointer, keyboard, and focus input.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    /// Disable pointer, keyboard, and focus input.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the editor accepts user input.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Set whether the editor accepts pointer, keyboard, and focus input.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.context_menu = None;
            if self.has_active_pointer_interaction() {
                self.pending_capture_release = true;
            }
            if let Some(pan) = self.panning.take() {
                self.viewport = pan.origin;
            }
            self.cancel_drag_state();
            self.focused = false;
            self.focus_visible = false;
        }
    }

    /// Current curve points in monotonic-x order.
    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    /// Replace curve points. Times are bounded and sorted; temporal values stay absolute.
    pub fn set_points(&mut self, points: Vec<CurvePoint>) {
        // Replacement supplies snapshot geometry, so no old authored ID owns a slot.
        if !self.selected_keys.is_empty()
            || self
                .selected
                .and_then(|index| self.point_target(index))
                .is_some_and(|target| matches!(target, CurvePointTarget::Keyframe(_)))
        {
            self.selected = None;
        }
        self.selected_keys.clear();
        if self.has_active_pointer_interaction() {
            self.pending_capture_release = true;
        }
        self.dragging = None;
        self.tangent_drag = None;
        if let Some(pan) = self.panning.take() {
            self.viewport = pan.origin;
        }
        self.tangents.clear();
        self.selected_tangent = None;
        self.tangent_origin = None;
        self.points = normalize_points(points, self.value_axis.is_some());
        Self::constrain_values(&mut self.points, self.fixed_value);
        self.point_metadata = default_point_policies(self.points.len())
            .into_iter()
            .enumerate()
            .map(|(index, policy)| CurvePointMetadata {
                target: CurvePointTarget::Snapshot(index),
                policy,
                context_menu: None,
            })
            .collect();
        self.rebuild_keyframe_indices();
        self.context_menu = None;
        self.selected = self.selected.filter(|index| *index < self.points.len());
    }

    /// Attach exact author key identities to the current ordered points.
    /// `None` denotes an evaluated viewport boundary, never another key's slot.
    /// A mismatched length or duplicate author ID leaves all identities unchanged.
    pub fn with_keyframe_ids(mut self, ids: Vec<Option<mondrian_core::KeyframeId>>) -> Self {
        let mut seen = std::collections::HashSet::new();
        if ids.len() != self.points.len() || ids.iter().flatten().any(|id| !seen.insert(*id)) {
            return self;
        }
        for (index, (metadata, id)) in self.point_metadata.iter_mut().zip(ids).enumerate() {
            metadata.target = id.map_or(
                CurvePointTarget::VirtualBoundary(if self.points[index].x == 0.0 {
                    CurveBoundary::Start
                } else {
                    CurveBoundary::End
                }),
                CurvePointTarget::Keyframe,
            );
        }
        self.rebuild_keyframe_indices();
        self
    }

    /// Stable source of the point currently occupying this display slot.
    pub fn point_target(&self, index: usize) -> Option<CurvePointTarget> {
        self.point_metadata.get(index).map(|metadata| metadata.target)
    }

    fn rebuild_keyframe_indices(&mut self) {
        self.keyframe_indices.clear();
        self.keyframe_indices.extend(self.point_metadata.iter().enumerate().filter_map(
            |(index, metadata)| match metadata.target {
                CurvePointTarget::Keyframe(id) => Some((id, index)),
                _ => None,
            },
        ));
    }

    fn reorder_temporal_points(
        &mut self,
        canonical_order: Option<&[(mondrian_core::KeyframeId, CurvePoint)]>,
    ) {
        if self.value_axis.is_none() {
            return;
        }
        // Reuse the ID map as canonical ranks while sorting. Exact owner order
        // resolves distinct times that collapse to the same f32 projection.
        if let Some(order) = canonical_order {
            for (rank, (id, _)) in order.iter().enumerate() {
                if let Some(index) = self.keyframe_indices.get_mut(id) {
                    *index = rank;
                }
            }
        }
        let mut reordered = false;
        for end in 1..self.points.len() {
            let mut right = end;
            while right > 0 {
                let left = right - 1;
                let mut ordering = self.points[left].x.total_cmp(&self.points[right].x);
                if ordering.is_eq()
                    && canonical_order.is_some()
                    && let (CurvePointTarget::Keyframe(a), CurvePointTarget::Keyframe(b)) = (
                        self.point_metadata[left].target,
                        self.point_metadata[right].target,
                    )
                {
                    ordering = self.keyframe_indices.get(&a).cmp(&self.keyframe_indices.get(&b));
                }
                if !ordering.is_gt() {
                    break;
                }
                self.points.swap(left, right);
                self.point_metadata.swap(left, right);
                if let Some(selected) = self.selected.as_mut() {
                    if *selected == left {
                        *selected = right;
                    } else if *selected == right {
                        *selected = left;
                    }
                }
                if let Some(drag) = self.dragging.as_mut() {
                    if drag.index == left {
                        drag.index = right;
                    } else if drag.index == right {
                        drag.index = left;
                    }
                }
                reordered = true;
                right = left;
            }
        }
        if reordered || canonical_order.is_some() {
            self.rebuild_keyframe_indices();
        }
    }

    fn reconcile_virtual_boundaries(&mut self, boundaries: [Option<CurvePoint>; 2]) {
        if self.value_axis.is_none() {
            return;
        }
        let mut shape_changed = false;
        for (boundary, point) in
            [CurveBoundary::Start, CurveBoundary::End].into_iter().zip(boundaries)
        {
            let target = CurvePointTarget::VirtualBoundary(boundary);
            let index = self.point_metadata.iter().position(|metadata| metadata.target == target);
            match (index, point) {
                (Some(index), Some(point)) => self.points[index] = point,
                (Some(index), None) => {
                    shape_changed = true;
                    self.points.remove(index);
                    self.point_metadata.remove(index);
                    self.selected = self.selected.and_then(|selected| {
                        if selected == index {
                            None
                        } else {
                            Some(selected - usize::from(selected > index))
                        }
                    });
                    if let Some(drag) = self.dragging.as_mut() {
                        drag.index -= usize::from(drag.index > index);
                    }
                }
                (None, Some(point)) => {
                    shape_changed = true;
                    let index = if boundary == CurveBoundary::Start {
                        0
                    } else {
                        self.points.len()
                    };
                    self.points.insert(index, point);
                    self.point_metadata.insert(
                        index,
                        CurvePointMetadata {
                            target,
                            policy: CurvePointPolicy::anchor(),
                            context_menu: None,
                        },
                    );
                    if let Some(selected) = self.selected.as_mut() {
                        *selected += usize::from(*selected >= index);
                    }
                    if let Some(drag) = self.dragging.as_mut() {
                        drag.index += usize::from(drag.index >= index);
                    }
                }
                (None, None) => {}
            }
        }
        if shape_changed {
            self.rebuild_keyframe_indices();
        }
    }

    fn restore_drag_boundaries(&mut self, drag: CurvePointDrag) -> bool {
        let Some(origin) = self.drag_boundary_origin.take() else {
            return false;
        };
        let selected_target = self
            .selected
            .and_then(|index| self.point_metadata.get(index))
            .map(|metadata| metadata.target);
        for index in (0..self.points.len()).rev() {
            if matches!(
                self.point_metadata[index].target,
                CurvePointTarget::VirtualBoundary(_)
            ) {
                self.points.remove(index);
                self.point_metadata.remove(index);
            }
        }
        if !drag.inserted
            && !matches!(drag.target_origin, CurvePointTarget::VirtualBoundary(_))
            && let Some(index) = self
                .point_metadata
                .iter()
                .position(|metadata| metadata.target == drag.target_origin)
        {
            let rank = drag.origin_index
                - origin
                    .iter()
                    .flatten()
                    .filter(|(index, _, _)| *index < drag.origin_index)
                    .count();
            if index < rank {
                self.points[index..=rank].rotate_left(1);
                self.point_metadata[index..=rank].rotate_left(1);
            } else if index > rank {
                self.points[rank..=index].rotate_right(1);
                self.point_metadata[rank..=index].rotate_right(1);
            }
        }
        for (index, point, metadata) in origin.into_iter().flatten() {
            self.points.insert(index, point);
            self.point_metadata.insert(index, metadata);
        }
        self.selected = selected_target.and_then(|target| {
            self.point_metadata.iter().position(|metadata| metadata.target == target)
        });
        self.rebuild_keyframe_indices();
        true
    }

    fn selected_keyframe(&self) -> Option<mondrian_core::KeyframeId> {
        match self.point_target(self.selected?)? {
            CurvePointTarget::Keyframe(id) => Some(id),
            _ => None,
        }
    }

    /// Attach an owner key for preserving view state across shell rebuilds.
    /// The shell must also scope restoration to the current author session.
    pub fn with_view_key(mut self, key: Option<String>) -> Self {
        self.view_key = key;
        self
    }

    /// Route horizontal arrows as exact temporal intents rather than screen ratios.
    /// Nonpositive steps consume horizontal keys without authoring time changes.
    pub fn with_frame_time_step(mut self, step: mondrian_core::TimelineTime) -> Self {
        self.frame_time_step = Some(step);
        self
    }

    /// Attach localized accessible naming and keyboard instructions.
    pub fn with_keyboard_help(mut self, name: impl Into<String>, help: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self.keyboard_help = Some(help.into());
        self
    }

    /// Capture inactive selection and focus; live drafts are never transferable.
    pub fn view_state(&self) -> Option<CurveEditorViewState> {
        if self.has_active_pointer_interaction()
            || self.gesture.is_some()
            || self.context_menu.is_some()
        {
            return None;
        }
        let selected = self.selected.and_then(|index| self.point_target(index));
        Some(CurveEditorViewState {
            viewport: self.viewport,
            view_key: self.view_key.clone()?,
            id: self.id,
            points: selected
                .filter(|target| !matches!(target, CurvePointTarget::Keyframe(_)))
                .map(|_| self.points.clone()),
            selected,
            selected_keys: self.selected_keys.clone(),
            selected_tangent: self.selected_tangent,
            focused: self.focused,
            focus_visible: self.focus_visible,
        })
    }

    /// Restore only an inactive view with the same owner key.
    /// Missing author keys lose selection; snapshot points require identical geometry.
    /// Retaining the widget ID lets the router resolve focus in the new tree.
    pub fn restore_view_state(&mut self, state: &CurveEditorViewState) -> bool {
        if self.view_key.as_deref() != Some(state.view_key())
            || self.has_active_pointer_interaction()
            || self.gesture.is_some()
            || self.context_menu.is_some()
        {
            return false;
        }
        self.viewport = state.viewport;
        self.id = state.id;
        self.selected = state.selected.and_then(|target| match target {
            CurvePointTarget::Keyframe(id) => self.keyframe_indices.get(&id).copied(),
            _ if state.points.as_deref() == Some(self.points.as_slice()) => {
                self.point_metadata.iter().position(|point| point.target == target)
            }
            _ => None,
        });
        self.selected_keys = state
            .selected_keys
            .iter()
            .copied()
            .filter(|id| self.keyframe_indices.contains_key(id))
            .collect();
        if self.selected.is_none() {
            self.selected = self.point_metadata.iter().position(|metadata| {
                matches!(metadata.target,
                CurvePointTarget::Keyframe(id) if self.selected_keys.contains(&id))
            });
        }
        self.selected_tangent = state.selected_tangent.filter(|(id, side)| {
            Some(*id) == self.selected_keyframe()
                && self
                    .tangents
                    .iter()
                    .any(|tangent| tangent.keyframe_id == *id && tangent.side == *side)
        });
        self.focused = state.focused && self.enabled;
        self.focus_visible = state.focus_visible && self.enabled;
        true
    }

    /// Selected point index, if any.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Enable stable authored-key selection and atomic selection deletion.
    /// The owner adapter must admit `DeleteMany` through its canonical transaction.
    pub fn with_selection_deletion(mut self) -> Self {
        self.selection_deletion = true;
        self
    }

    /// Admit exact keyboard time movement of a stable authored-key selection.
    /// The owner adapter must implement `NudgeTimeMany` as one canonical transaction.
    pub fn with_selection_time_movement(mut self) -> Self {
        self.selection_time_movement = true;
        self
    }

    /// Enable selected-key value increments admitted as one owner transaction.
    /// The adapter must implement `NudgeValueMany` using canonical values and bounds.
    pub fn with_selection_value_movement(mut self) -> Self {
        self.selection_value_movement = true;
        self
    }

    /// Selected authored keys in current canonical display order.
    pub fn selected_keyframe_ids(&self) -> Vec<mondrian_core::KeyframeId> {
        self.point_metadata
            .iter()
            .filter_map(|metadata| match metadata.target {
                CurvePointTarget::Keyframe(id) if self.selected_keys.contains(&id) => Some(id),
                _ => None,
            })
            .collect()
    }

    fn toggle_key_selection(&mut self, index: usize) {
        let Some(CurvePointTarget::Keyframe(id)) = self.point_target(index) else {
            return;
        };
        if !self.selected_keys.remove(&id) {
            self.selected_keys.insert(id);
            self.selected = Some(index);
        } else if self.selected == Some(index) {
            self.selected = self.point_metadata.iter().position(|metadata| matches!(
                metadata.target, CurvePointTarget::Keyframe(id) if self.selected_keys.contains(&id)
            ));
        }
        self.selected_tangent = None;
    }

    /// Select a point by index.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index.filter(|index| *index < self.points.len());
        self.selected_keys.clear();
        if let Some(id) = self.selected_keyframe() {
            self.selected_keys.insert(id);
        }
        self.selected_tangent = None;
    }

    /// Attach controls projected by the owner evaluator, without changing author state.
    pub fn with_tangents(mut self, tangents: Vec<CurveTangent>) -> Self {
        self.set_tangents(tangents);
        self
    }

    fn set_tangents(&mut self, tangents: Vec<CurveTangent>) {
        self.tangents = tangents
            .into_iter()
            .filter(|tangent| {
                self.keyframe_indices.contains_key(&tangent.keyframe_id)
                    && tangent.point.x.is_finite()
                    && tangent.point.y.is_finite()
                    && tangent.time_limits.iter().all(|value| value.is_finite())
                    && tangent.time_limits[0] <= tangent.time_limits[1]
            })
            .collect();
    }

    fn hit_tangent(&self, position: Point) -> Option<CurveTangent> {
        if !self.plot_rect().contains(position) {
            return None;
        }
        let tangent = self
            .tangents
            .iter()
            .filter(|tangent| Some(tangent.keyframe_id) == self.selected_keyframe())
            .copied()
            .filter(|tangent| {
                let screen = self.to_screen(tangent.point);
                if !self.plot_rect().contains(screen) {
                    return false;
                }
                let dx = screen.x - position.x;
                let dy = screen.y - position.y;
                dx * dx + dy * dy <= HIT_RADIUS * HIT_RADIUS
            })
            .min_by(|a, b| {
                let distance = |tangent: &CurveTangent| {
                    let screen = self.to_screen(tangent.point);
                    (screen.x - position.x).hypot(screen.y - position.y)
                };
                distance(a).total_cmp(&distance(b))
            })?;
        if let Some(index) = self.hit_point(position) {
            let distance = |point| {
                let screen = self.to_screen(point);
                (screen.x - position.x).hypot(screen.y - position.y)
            };
            if distance(self.points[index]) <= distance(tangent.point) {
                return None;
            }
        }
        Some(tangent)
    }

    fn move_tangent(
        &mut self,
        tangent: CurveTangent,
        mut point: CurvePoint,
        ctx: &mut EventContext,
    ) -> bool {
        if !point.x.is_finite() || !point.y.is_finite() {
            return false;
        }
        point.x = point.x.clamp(tangent.time_limits[0], tangent.time_limits[1]);
        let Some(current) = self.tangents.iter_mut().find(|current| {
            current.keyframe_id == tangent.keyframe_id && current.side == tangent.side
        }) else {
            return false;
        };
        if current.point == point {
            return false;
        }
        let previous = current.point;
        current.point = point;
        let edit = CurveEdit::Handle {
            keyframe_id: tangent.keyframe_id,
            side: tangent.side,
            point,
        };
        let accepted = if self.tangent_drag.is_some() {
            self.dispatch_gesture(
                CurveGestureEvent::Edit { gesture_id: self.gesture, edit: edit.clone() },
                ctx,
            )
        } else {
            self.dispatch_edit(edit, ctx)
        };
        if !accepted {
            if let Some(current) = self.tangents.iter_mut().find(|current| {
                current.keyframe_id == tangent.keyframe_id && current.side == tangent.side
            }) {
                current.point = previous;
            }
            return false;
        }
        ctx.request_repaint();
        true
    }

    fn geometry(&self) -> CurveGeometry {
        CurveGeometry::new(self.bounds, self.viewport).with_ruler_insets(self.ruler_insets)
    }

    fn plot_rect(&self) -> Rect {
        self.geometry().plot_rect()
    }

    fn to_screen(&self, point: CurvePoint) -> Point {
        self.geometry().curve_to_screen(point)
    }

    fn hit_point(&self, position: Point) -> Option<usize> {
        self.geometry().hit_point(&self.points, position)
    }

    fn value_step(&self, coarse: bool) -> f64 {
        self.value_axis
            .and_then(|axis| axis.step)
            .unwrap_or((if coarse { 0.1 } else { 0.02 }) * self.viewport.value_radius)
            * if coarse && self.value_axis.is_some_and(|axis| axis.step.is_some()) {
                5.0
            } else {
                1.0
            }
    }

    fn point_spacing(&self) -> CurvePointSpacing {
        if self.value_axis.is_some() {
            CurvePointSpacing::Temporal
        } else {
            CurvePointSpacing::Transfer
        }
    }

    fn constrained_point(&self, index: usize, point: CurvePoint) -> CurvePoint {
        let policy = self.point_metadata.get(index).map(|metadata| metadata.policy);
        let lock_x = policy.is_none_or(|policy| policy.lock_x);
        let point = if policy.is_none_or(|policy| policy.lock_y) {
            CurvePoint {
                y: self.points.get(index).map_or(point.y, |current| current.y),
                ..point
            }
        } else {
            point
        };
        constrained_point(
            &self.points,
            index,
            self.constrained_value(point),
            lock_x,
            self.point_spacing(),
        )
    }

    fn move_point(&mut self, index: usize, point: CurvePoint) -> bool {
        if point.x.is_nan() || point.y.is_nan() {
            return false;
        }
        let Some(current) = self.points.get(index).copied() else {
            return false;
        };
        let next = self.constrained_point(index, point);
        if current == next {
            return false;
        }
        self.points[index] = next;
        true
    }

    fn move_point_from_input(
        &mut self,
        index: usize,
        point: CurvePoint,
        ctx: &mut EventContext,
    ) -> bool {
        let Some(previous) = self.points.get(index).copied() else {
            return false;
        };
        if !self.move_point(index, point) {
            return false;
        }
        if self.on_gesture.is_some()
            && let Some(drag) = self.dragging
            && !self.dispatch_gesture(
                CurveGestureEvent::Edit {
                    gesture_id: self.gesture,
                    edit: self.drag_author_edit(drag),
                },
                ctx,
            )
        {
            self.points[index] = previous;
            return false;
        }
        if self.dragging.is_some() {
            if self.on_gesture.is_none() {
                self.reorder_temporal_points(None);
            }
            self.dispatch_change(ctx);
        }
        ctx.request_repaint();
        true
    }

    fn dispatch_change(&self, ctx: &mut EventContext) {
        if let Some(action) = &self.on_value_edit
            && let Some(action) = action(
                self.gesture.map_or(ValueEdit::Single, ValueEdit::Update),
                &self.points,
            )
        {
            (ctx.dispatch)(action);
        }
        if let Some(action) = &self.on_change
            && let Some(action) = action(&self.points)
        {
            (ctx.dispatch)(action);
        }
    }

    fn dispatch_gesture(&mut self, event: CurveGestureEvent, ctx: &mut EventContext) -> bool {
        let points = self.preview_projection.as_mut().and_then(|project| project(event.clone()));
        if matches!(event, CurveGestureEvent::Edit { .. })
            && self.preview_projection.is_some()
            && points.is_none()
        {
            return false;
        }
        if matches!(event, CurveGestureEvent::Edit { .. })
            && let Some(points) = points
        {
            for (id, point) in &points.key_points {
                if let Some(index) = self.keyframe_indices.get(id).copied() {
                    self.points[index] = *point;
                }
            }
            self.reorder_temporal_points(Some(&points.key_points));
            self.reconcile_virtual_boundaries(points.virtual_boundaries);
            self.set_display_points(points.display_points);
            self.set_tangents(points.tangents);
            if !self.has_active_pointer_interaction() {
                self.reveal_selection();
            }
            ctx.request_repaint();
        }
        if matches!(event, CurveGestureEvent::Edit { .. }) && self.preview_projection.is_none() {
            self.reorder_temporal_points(None);
        }
        if let Some(callback) = &self.on_gesture
            && let Some(action) = callback(event)
        {
            (ctx.dispatch)(action);
        }
        true
    }

    fn end_gesture(&mut self, cancel: bool, ctx: &mut EventContext) {
        if let Some(origin) = self.tangent_origin.take()
            && cancel
        {
            self.tangents = origin;
        }
        if let Some(origin) = self.drag_display_origin.take()
            && cancel
        {
            self.display_points = origin;
        }
        self.drag_boundary_origin = None;
        if let Some(id) = self.gesture.take() {
            if let Some(action) = &self.on_value_edit
                && let Some(action) = action(
                    if cancel {
                        ValueEdit::Cancel(id)
                    } else {
                        ValueEdit::Finish(id)
                    },
                    &self.points,
                )
            {
                (ctx.dispatch)(action);
            }
            self.drag_value_origin = None;
            self.dispatch_gesture(
                if cancel {
                    CurveGestureEvent::Cancel(id)
                } else {
                    CurveGestureEvent::Finish(id)
                },
                ctx,
            );
        }
    }

    fn dispatch_edit(&mut self, edit: CurveEdit, ctx: &mut EventContext) -> bool {
        if !self.dispatch_gesture(
            CurveGestureEvent::Edit { gesture_id: self.gesture, edit: edit.clone() },
            ctx,
        ) {
            return false;
        }
        if let Some(action) = &self.on_edit
            && let Some(action) = action(edit)
        {
            (ctx.dispatch)(action);
        }
        true
    }

    fn begin_value_gesture(&mut self, ctx: &mut EventContext) {
        if self.on_value_edit.is_none() {
            return;
        }
        self.commit_drag_edit(ctx);
        self.end_gesture(false, ctx);
        self.drag_value_origin = Some((self.points.clone(), self.point_metadata.clone()));
        let id = mondrian_core::AuthoringGestureId::new();
        self.gesture = Some(id);
        if let Some(action) = &self.on_value_edit
            && let Some(action) = action(ValueEdit::Begin(id), &self.points)
        {
            (ctx.dispatch)(action);
        }
    }

    fn begin_drag(
        &mut self,
        index: usize,
        inserted: bool,
        pointer_origin: Point,
        ctx: &mut EventContext,
    ) {
        if self.on_value_edit.is_some() {
            if !inserted {
                self.begin_value_gesture(ctx);
            }
        } else {
            self.end_gesture(true, ctx);
        }
        if self.value_axis.is_some()
            && (self.on_gesture.is_some() || self.preview_projection.is_some())
        {
            self.drag_boundary_origin = Some(std::array::from_fn(|side| {
                let target = CurvePointTarget::VirtualBoundary(if side == 0 {
                    CurveBoundary::Start
                } else {
                    CurveBoundary::End
                });
                self.point_metadata
                    .iter()
                    .enumerate()
                    .find(|(_, metadata)| metadata.target == target)
                    .map(|(boundary_index, metadata)| {
                        (
                            boundary_index - usize::from(inserted && boundary_index > index),
                            self.points[boundary_index],
                            metadata.clone(),
                        )
                    })
            }));
        }
        if self.on_gesture.is_some() {
            self.drag_display_origin = Some(self.display_points.clone());
            let id = mondrian_core::AuthoringGestureId::new();
            self.gesture = Some(id);
            self.dispatch_gesture(CurveGestureEvent::Begin(id), ctx);
        }
        self.tangent_origin = Some(self.tangents.clone());
        let target_origin = self.point_metadata[index].target;
        let keyframe_id = match target_origin {
            CurvePointTarget::Keyframe(id) => id,
            _ => mondrian_core::KeyframeId::new(),
        };
        if matches!(target_origin, CurvePointTarget::VirtualBoundary(_)) {
            self.point_metadata[index].target = CurvePointTarget::Keyframe(keyframe_id);
            self.rebuild_keyframe_indices();
        }
        self.dragging = Some(CurvePointDrag {
            index,
            origin_index: index,
            origin: self.points[index],
            pointer_origin,
            inserted,
            target_origin,
            keyframe_id,
        });
        if inserted
            && self.on_gesture.is_some()
            && let Some(drag) = self.dragging
            && !self.dispatch_gesture(
                CurveGestureEvent::Edit {
                    gesture_id: self.gesture,
                    edit: self.drag_author_edit(drag),
                },
                ctx,
            )
        {
            self.cancel_drag_state();
            self.selected = None;
            self.end_gesture(true, ctx);
        }
    }

    fn drag_author_edit(&self, drag: CurvePointDrag) -> CurveEdit {
        let point = self.points[drag.index];
        if drag.inserted || matches!(drag.target_origin, CurvePointTarget::VirtualBoundary(_)) {
            CurveEdit::Insert {
                keyframe_id: drag.keyframe_id,
                index: drag.index,
                point,
            }
        } else {
            CurveEdit::Move {
                target: self.point_metadata[drag.index].target,
                point,
            }
        }
    }

    fn commit_drag_edit(&mut self, ctx: &mut EventContext) {
        if let Some(drag) = self.tangent_drag.take() {
            if self.on_gesture.is_none()
                && let Some(tangent) = self
                    .tangents
                    .iter()
                    .find(|tangent| {
                        tangent.keyframe_id == drag.tangent.keyframe_id
                            && tangent.side == drag.tangent.side
                    })
                    .copied()
                && tangent.point != drag.tangent.point
            {
                self.dispatch_edit(
                    CurveEdit::Handle {
                        keyframe_id: tangent.keyframe_id,
                        side: tangent.side,
                        point: tangent.point,
                    },
                    ctx,
                );
            }
            self.end_gesture(false, ctx);
            return;
        }
        let Some(drag) = self.dragging.take() else {
            return;
        };
        let point = self.points.get(drag.index).copied();
        if drag.inserted {
            if let Some(point) = point {
                self.dispatch_edit(
                    CurveEdit::Insert {
                        keyframe_id: drag.keyframe_id,
                        index: drag.index,
                        point,
                    },
                    ctx,
                );
            }
        } else if let Some(point) = point
            && drag.origin != point
        {
            let edit = self.drag_author_edit(drag);
            self.dispatch_edit(edit, ctx);
        }
        let virtual_boundary = matches!(drag.target_origin, CurvePointTarget::VirtualBoundary(_));
        let unchanged = point == Some(drag.origin);
        let index = self.keyframe_indices.get(&drag.keyframe_id).copied().or_else(|| {
            self.point_metadata
                .iter()
                .position(|metadata| metadata.target == drag.target_origin)
        });
        if virtual_boundary && let Some(index) = index {
            if unchanged {
                self.point_metadata[index].target = drag.target_origin;
                self.rebuild_keyframe_indices();
            } else {
                self.point_metadata[index].policy = CurvePointPolicy::editable();
            }
        }
        self.end_gesture(virtual_boundary && unchanged, ctx);
    }

    fn cancel_drag_state(&mut self) -> bool {
        if self.tangent_drag.take().is_some() {
            if let Some(origin) = &self.tangent_origin {
                self.tangents = origin.clone();
            }
            return true;
        }
        let Some(drag) = self.dragging.take() else {
            return false;
        };
        if let Some((points, policies)) = self.drag_value_origin.take() {
            let changed = self.points != points;
            self.points = points;
            self.point_metadata = policies;
            self.rebuild_keyframe_indices();
            self.selected = if drag.inserted {
                None
            } else {
                self.point_metadata
                    .iter()
                    .position(|metadata| metadata.target == drag.target_origin)
            };
            return changed;
        }
        if drag.inserted {
            if drag.index < self.points.len() {
                self.points.remove(drag.index);
                self.point_metadata.remove(drag.index);
                self.selected = None;
                self.restore_drag_boundaries(drag);
                self.rebuild_keyframe_indices();
                true
            } else {
                false
            }
        } else {
            self.point_metadata[drag.index].target = drag.target_origin;
            let selected_target = self
                .selected
                .and_then(|index| self.point_metadata.get(index))
                .map(|metadata| metadata.target);
            let changed = self
                .points
                .get_mut(drag.index)
                .map(|point| {
                    let changed = *point != drag.origin || drag.index != drag.origin_index;
                    *point = drag.origin;
                    changed
                })
                .unwrap_or(false);
            if self.restore_drag_boundaries(drag) {
                return changed;
            }
            // Restore the original slot rather than sorting projected coordinates:
            // distinct exact owner times can share the same f32 position.
            if drag.index < drag.origin_index {
                self.points[drag.index..=drag.origin_index].rotate_left(1);
                self.point_metadata[drag.index..=drag.origin_index].rotate_left(1);
            } else if drag.index > drag.origin_index {
                self.points[drag.origin_index..=drag.index].rotate_right(1);
                self.point_metadata[drag.origin_index..=drag.index].rotate_right(1);
            }
            self.selected = selected_target.and_then(|target| {
                self.point_metadata.iter().position(|metadata| metadata.target == target)
            });
            self.rebuild_keyframe_indices();
            changed
        }
    }

    fn reveal_selection(&mut self) {
        let point = self
            .selected_tangent
            .and_then(|(id, side)| {
                self.tangents
                    .iter()
                    .find(|t| t.keyframe_id == id && t.side == side)
                    .map(|t| t.point)
            })
            .or_else(|| self.selected.and_then(|index| self.points.get(index).copied()));
        if let Some(point) = point {
            self.viewport.reveal(point);
        }
    }

    fn viewport_keyboard(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        ctx: &mut EventContext,
    ) -> bool {
        if ctx.focus.focused_widget() != Some(self.id)
            || !self.focused
            || !(modifiers.ctrl ^ modifiers.meta)
            || modifiers.alt
        {
            return false;
        }
        let zoom = matches!(key, KeyCode::PageUp | KeyCode::PageDown);
        let reset = key == KeyCode::Digit0 && !modifiers.shift;
        let pan = modifiers.shift
            && matches!(
                key,
                KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
            );
        if !zoom && !reset && !pan {
            return false;
        }
        if self.has_active_pointer_interaction() {
            return true;
        }
        if reset {
            self.viewport = self.value_axis.map_or_else(CurveViewport::default, |axis| {
                CurveViewport::with_value_range(axis.initial_range())
            });
        } else if zoom {
            self.viewport.zoom(
                [0.5, 0.5],
                if key == KeyCode::PageUp { 0.8 } else { 1.25 },
                modifiers.shift,
            );
        } else {
            self.viewport.pan(match key {
                KeyCode::Left => [-0.1, 0.0],
                KeyCode::Right => [0.1, 0.0],
                KeyCode::Up => [0.0, 0.1],
                _ => [0.0, -0.1],
            });
        }
        ctx.request_repaint();
        true
    }

    fn navigate_keyboard(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        ctx: &mut EventContext,
    ) -> bool {
        if ctx.focus.focused_widget() != Some(self.id) || !self.focused {
            return false;
        }
        let primary = (modifiers.ctrl ^ modifiers.meta) && !modifiers.alt && !modifiers.shift;
        let alt = modifiers.alt && !modifiers.ctrl && !modifiers.meta && !modifiers.shift;
        let plain = modifiers == Modifiers::none();
        let menu = key == KeyCode::F10 && modifiers == Modifiers::shift();
        let navigation = (plain && matches!(key, KeyCode::Home | KeyCode::End))
            || (primary && matches!(key, KeyCode::Left | KeyCode::Right))
            || (alt && matches!(key, KeyCode::Left | KeyCode::Right | KeyCode::Up));
        if !navigation && !menu {
            return false;
        }
        // Selection cannot change the owner of an outstanding pointer draft.
        if self.has_active_pointer_interaction() || self.gesture.is_some() {
            return true;
        }
        if menu && self.selection_menu.is_some() {
            if let Some(index) = self.selected {
                self.open_selection_menu(self.to_screen(self.points[index]), true, ctx);
            }
            return true;
        }
        if menu && self.selected_keys.len() > 1 {
            return true;
        }
        if menu {
            if let Some(index) = self.selected
                && let Some(items) = self.point_metadata[index].context_menu.as_ref()
                && items.iter().any(MenuItem::is_activatable)
            {
                let mut menu = ContextMenu::new(self.to_screen(self.points[index]), items.clone());
                menu.layout(self.bounds);
                menu.event(
                    &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
                    ctx,
                );
                self.context_menu = Some(menu);
                ctx.tooltip.hide();
                ctx.request_repaint();
            }
            return true;
        }
        if self.points.is_empty() {
            return true;
        }
        let before = (
            self.selected,
            self.selected_tangent,
            self.focus_visible,
            self.viewport,
        );
        self.focus_visible = true;
        if alt && self.selected_keys.len() > 1 {
            return true;
        }
        if alt {
            if key == KeyCode::Up {
                self.selected_tangent = None;
            } else if let Some(id) = self.selected_keyframe() {
                let side = if key == KeyCode::Left {
                    mondrian_core::BezierHandleSide::Incoming
                } else {
                    mondrian_core::BezierHandleSide::Outgoing
                };
                if self
                    .tangents
                    .iter()
                    .any(|tangent| tangent.keyframe_id == id && tangent.side == side)
                {
                    self.selected_tangent = Some((id, side));
                }
            }
        } else {
            self.selected = Some(match key {
                KeyCode::Home => 0,
                KeyCode::End => self.points.len() - 1,
                KeyCode::Left => {
                    self.selected.map_or(self.points.len() - 1, |index| index.saturating_sub(1))
                }
                _ => self.selected.map_or(0, |index| (index + 1).min(self.points.len() - 1)),
            });
            self.selected_tangent = None;
        }
        if !alt {
            self.selected_keys.clear();
            if let Some(id) = self.selected_keyframe() {
                self.selected_keys.insert(id);
            }
        }
        self.reveal_selection();
        if before
            != (
                self.selected,
                self.selected_tangent,
                self.focus_visible,
                self.viewport,
            )
        {
            ctx.request_repaint();
        }
        true
    }

    fn nudge_selected(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        ctx: &mut EventContext,
    ) -> bool {
        if self.selected_keys.len() > 1 {
            if !matches!(
                key,
                KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
            ) {
                return false;
            }
            if self.selection_value_movement
                && self.value_axis.is_some()
                && self.fixed_value.is_none()
                && !self.has_active_pointer_interaction()
                && matches!(key, KeyCode::Up | KeyCode::Down)
                && !modifiers.ctrl
                && !modifiers.alt
                && !modifiers.meta
            {
                let keyframe_ids = self.selected_keyframe_ids();
                let movable = keyframe_ids.len() == self.selected_keys.len()
                    && keyframe_ids.iter().all(|id| {
                        self.keyframe_indices
                            .get(id)
                            .is_some_and(|index| !self.point_metadata[*index].policy.lock_y)
                    });
                let delta = self.value_step(false);
                let steps = if modifiers.shift { 5 } else { 1 };
                let steps = if key == KeyCode::Down { -steps } else { steps };
                if movable && delta.is_finite() && delta != 0.0 {
                    self.dispatch_edit(
                        CurveEdit::NudgeValueMany { keyframe_ids, delta, steps },
                        ctx,
                    );
                }
            }
            if self.selection_time_movement
                && !self.has_active_pointer_interaction()
                && matches!(key, KeyCode::Left | KeyCode::Right)
                && !modifiers.ctrl
                && !modifiers.alt
                && !modifiers.meta
                && let Some(step) = self.frame_time_step
                && step > mondrian_core::TimelineTime::ZERO
            {
                let keyframe_ids = self.selected_keyframe_ids();
                let movable = keyframe_ids.len() == self.selected_keys.len()
                    && keyframe_ids.iter().all(|id| {
                        self.keyframe_indices
                            .get(id)
                            .is_some_and(|index| !self.point_metadata[*index].policy.lock_x)
                    });
                let count = if modifiers.shift { 5 } else { 1 };
                let count = if key == KeyCode::Left { -count } else { count };
                if movable
                    && let Ok(scale) = mondrian_core::TimeScale::new(count, 1)
                    && let Ok(delta) = step.checked_scale(scale)
                {
                    self.dispatch_edit(CurveEdit::NudgeTimeMany { keyframe_ids, delta }, ctx);
                }
            }
            return true;
        }
        if let Some(step) = self.frame_time_step
            && matches!(key, KeyCode::Left | KeyCode::Right)
            && !modifiers.ctrl
            && !modifiers.alt
            && !modifiers.meta
        {
            if self.has_active_pointer_interaction() || step <= mondrian_core::TimelineTime::ZERO {
                return true;
            }
            let target = self
                .selected_tangent
                .map(|(id, side)| (id, Some(side)))
                .or_else(|| self.selected_keyframe().map(|id| (id, None)));
            if let Some((keyframe_id, side)) = target {
                let count = if modifiers.shift { 5 } else { 1 };
                let count = if key == KeyCode::Left { -count } else { count };
                if let Ok(scale) = mondrian_core::TimeScale::new(count, 1)
                    && let Ok(delta) = step.checked_scale(scale)
                {
                    self.dispatch_edit(CurveEdit::NudgeTime { keyframe_id, side, delta }, ctx);
                }
            }
            return true;
        }
        if self.value_axis.is_some()
            && self.selected_tangent.is_none()
            && matches!(key, KeyCode::Up | KeyCode::Down)
            && !modifiers.ctrl
            && !modifiers.alt
            && !modifiers.meta
            && let Some(keyframe_id) = self.selected_keyframe()
        {
            if self.has_active_pointer_interaction()
                || self.fixed_value.is_some()
                || self
                    .keyframe_indices
                    .get(&keyframe_id)
                    .is_none_or(|index| self.point_metadata[*index].policy.lock_y)
            {
                return true;
            }
            let delta = self.value_step(false);
            let steps = if modifiers.shift { 5 } else { 1 };
            let steps = if key == KeyCode::Down { -steps } else { steps };
            if delta.is_finite() && delta != 0.0 {
                self.dispatch_edit(CurveEdit::NudgeValue { keyframe_id, delta, steps }, ctx);
            }
            return true;
        }
        if let Some((keyframe_id, side)) = self.selected_tangent {
            if modifiers.ctrl || modifiers.alt || modifiers.meta {
                return false;
            }
            let Some(tangent) = self
                .tangents
                .iter()
                .find(|tangent| tangent.keyframe_id == keyframe_id && tangent.side == side)
                .copied()
            else {
                return false;
            };
            let mut point = tangent.point;
            let step = if modifiers.shift { 0.05 } else { 0.01 };
            match key {
                KeyCode::Left => point.x -= step,
                KeyCode::Right => point.x += step,
                KeyCode::Up => point.y += self.value_step(modifiers.shift),
                KeyCode::Down => point.y -= self.value_step(modifiers.shift),
                _ => return false,
            }
            self.move_tangent(tangent, point, ctx);
            return true;
        }
        if modifiers.ctrl || modifiers.alt || modifiers.meta {
            return false;
        }
        let Some(index) = self.selected else {
            return false;
        };
        let Some(point) = self.points.get(index).copied() else {
            return false;
        };
        let step = if modifiers.shift { 0.05 } else { 0.01 };
        let mut next = point;
        match key {
            KeyCode::Left => next.x -= step,
            KeyCode::Right => next.x += step,
            KeyCode::Up => next.y += self.value_step(modifiers.shift),
            KeyCode::Down => next.y -= self.value_step(modifiers.shift),
            _ => return false,
        }
        let previous = self.points[index];
        let metadata = self.point_metadata[index].clone();
        if !self.move_point_from_input(index, next, ctx) {
            return true;
        }
        let point = self.points[index];
        if self.dragging.is_none() {
            let target = self.point_metadata[index].target;
            let edit = if matches!(target, CurvePointTarget::VirtualBoundary(_)) {
                let keyframe_id = mondrian_core::KeyframeId::new();
                self.point_metadata[index].target = CurvePointTarget::Keyframe(keyframe_id);
                self.point_metadata[index].policy = CurvePointPolicy::editable();
                self.rebuild_keyframe_indices();
                CurveEdit::Insert { keyframe_id, index, point }
            } else {
                CurveEdit::Move { target, point }
            };
            if !self.dispatch_edit(edit, ctx) {
                self.points[index] = previous;
                self.point_metadata[index] = metadata;
                self.rebuild_keyframe_indices();
                return true;
            }
            self.dispatch_change(ctx);
        }
        true
    }

    fn insertion_at(&self, position: Point) -> Option<(usize, CurvePoint)> {
        self.geometry()
            .insertion_at(&self.points, position, self.point_spacing())
            .map(|(index, point)| (index, self.constrained_value(point)))
    }

    fn insert_point_from_input(
        &mut self,
        position: Point,
        ctx: &mut EventContext,
    ) -> Option<usize> {
        let (index, point) = self.insertion_at(position)?;
        self.begin_value_gesture(ctx);
        self.points.insert(index, point);
        self.point_metadata.insert(
            index,
            CurvePointMetadata {
                target: CurvePointTarget::Keyframe(mondrian_core::KeyframeId::new()),
                policy: CurvePointPolicy::editable(),
                context_menu: None,
            },
        );
        self.rebuild_keyframe_indices();
        self.selected = Some(index);
        self.dispatch_change(ctx);
        ctx.request_repaint();
        Some(index)
    }

    fn delete_selected_from_input(&mut self, ctx: &mut EventContext) -> bool {
        if self.selected_keys.len() > 1 {
            if !self.selection_deletion
                || self.has_active_pointer_interaction()
                || self.gesture.is_some()
            {
                return true;
            }
            let keyframe_ids = self.selected_keyframe_ids();
            if keyframe_ids.iter().any(|id| {
                self.keyframe_indices
                    .get(id)
                    .is_none_or(|index| !self.point_metadata[*index].policy.deletable)
            }) {
                return true;
            }
            let original_points = self.points.clone();
            let original_metadata = self.point_metadata.clone();
            let original_selection = self.selected;
            let mut index = 0;
            self.points.retain(|_| {
                let keep = !matches!(self.point_metadata[index].target,
                    CurvePointTarget::Keyframe(id) if self.selected_keys.contains(&id));
                index += 1;
                keep
            });
            self.point_metadata.retain(|metadata| {
                !matches!(metadata.target,
                CurvePointTarget::Keyframe(id) if self.selected_keys.contains(&id))
            });
            self.selected = None;
            self.rebuild_keyframe_indices();
            if !self.dispatch_edit(CurveEdit::DeleteMany { keyframe_ids }, ctx) {
                self.points = original_points;
                self.point_metadata = original_metadata;
                self.selected = original_selection;
                self.rebuild_keyframe_indices();
                return true;
            }
            self.selected_keys.clear();
            self.selected_tangent = None;
            self.dispatch_change(ctx);
            ctx.request_repaint();
            return true;
        }
        let Some(index) = self.selected else {
            return false;
        };
        if !self.point_metadata.get(index).is_some_and(|metadata| metadata.policy.deletable) {
            return false;
        }

        if self.dragging.is_some_and(|drag| drag.inserted) {
            self.cancel_drag_state();
            self.selected = None;
            self.end_gesture(true, ctx);
            if self.on_value_edit.is_none() {
                self.dispatch_change(ctx);
            }
            ctx.release_pointer_capture(self.id);
            ctx.request_repaint();
            return true;
        }
        let drag = self.dragging.take();
        let selection = self.selected;
        let point = self.points.remove(index);
        let metadata = self.point_metadata.remove(index);
        let target = metadata.target;
        self.rebuild_keyframe_indices();
        self.selected = (!self.points.is_empty()).then(|| index.min(self.points.len() - 1));
        if !self.dispatch_edit(CurveEdit::Delete { target }, ctx) {
            self.points.insert(index, point);
            self.point_metadata.insert(index, metadata);
            self.selected = selection;
            self.dragging = drag;
            self.rebuild_keyframe_indices();
            return true;
        }
        self.selected_keys.clear();
        if let Some(id) = self.selected_keyframe() {
            self.selected_keys.insert(id);
        }
        self.dispatch_change(ctx);
        if drag.is_some() {
            self.end_gesture(false, ctx);
            ctx.release_pointer_capture(self.id);
        }
        ctx.request_repaint();
        true
    }
}

fn default_point_policies(point_count: usize) -> Vec<CurvePointPolicy> {
    (0..point_count)
        .map(|index| {
            if index == 0 || index + 1 == point_count {
                CurvePointPolicy::anchor()
            } else {
                CurvePointPolicy::editable()
            }
        })
        .collect()
}

impl Default for CurveEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for CurveEditor {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(DEFAULT_WIDTH, DEFAULT_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.ruler_insets = if self.time_axis.is_some() {
            let theme = mondrian_ui_theme::current_theme();
            (
                theme.spacing.curve_value_ruler_width,
                theme.typography.metadata.line_height + theme.spacing.sm,
            )
        } else {
            (0.0, 0.0)
        };
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let Some(menu) = self.context_menu.as_mut() {
            let result = menu.event(event, ctx);
            if !menu.is_visible() {
                self.context_menu = None;
                ctx.request_repaint();
            }
            if result == EventResult::Handled {
                return result;
            }
        }
        if self.pending_capture_release || (!self.enabled && self.has_active_pointer_interaction())
        {
            ctx.release_pointer_capture(self.id);
            ctx.set_cursor(mondrian_ui_core::widget::CursorRequest::Default);
            self.end_gesture(true, ctx);
            self.pending_capture_release = false;
        }
        if !self.enabled {
            self.focused = false;
            self.focus_visible = false;
            self.dragging = None;
            self.tangent_drag = None;
            self.panning = None;
            return EventResult::Ignored;
        }

        if let Some(pan) = self.panning {
            match event {
                UiEvent::MouseMove { position, .. } => {
                    let plot = self.plot_rect();
                    self.viewport = pan.origin;
                    if plot.width > 0.0 && plot.height > 0.0 {
                        self.viewport.pan([
                            -f64::from(position.x - pan.pointer_origin.x) / f64::from(plot.width),
                            f64::from(position.y - pan.pointer_origin.y) / f64::from(plot.height),
                        ]);
                    }
                    ctx.set_cursor(mondrian_ui_core::widget::CursorRequest::Grabbing);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                UiEvent::MouseUp { button: MouseButton::Middle, .. } => {
                    self.panning = None;
                    ctx.set_cursor(mondrian_ui_core::widget::CursorRequest::Default);
                    ctx.release_pointer_capture(self.id);
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                    self.viewport = pan.origin;
                    self.panning = None;
                    ctx.set_cursor(mondrian_ui_core::widget::CursorRequest::Default);
                    ctx.release_pointer_capture(self.id);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                UiEvent::FocusLost => {
                    self.panning = None;
                    ctx.set_cursor(mondrian_ui_core::widget::CursorRequest::Default);
                    ctx.release_pointer_capture(self.id);
                }
                UiEvent::KeyDown { key: KeyCode::Tab, .. } => {}
                UiEvent::KeyDown { .. }
                | UiEvent::MouseDown { .. }
                | UiEvent::MouseWheel { .. } => return EventResult::Handled,
                _ => {}
            }
        }

        if matches!(event, UiEvent::KeyDown { .. }) {
            ctx.tooltip.hide();
        }
        match event {
            UiEvent::MouseWheel { position, delta, modifiers }
                if self.plot_rect().contains(*position)
                    && (modifiers.ctrl ^ modifiers.meta)
                    && !modifiers.alt =>
            {
                if !self.has_active_pointer_interaction() && delta.is_finite() {
                    let fraction = self.geometry().screen_fraction(*position);
                    self.viewport.zoom(
                        fraction,
                        (f64::from(*delta).clamp(-400.0, 400.0) / 100.0).exp(),
                        modifiers.shift,
                    );
                    ctx.tooltip.hide();
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            UiEvent::MouseDown { position, button: MouseButton::Middle, .. }
                if self.plot_rect().contains(*position) =>
            {
                if !self.has_active_pointer_interaction() {
                    self.panning =
                        Some(ViewportDrag { origin: self.viewport, pointer_origin: *position });
                    ctx.request_pointer_capture(self.id);
                    ctx.focus.request_focus(self.id);
                    ctx.tooltip.hide();
                    ctx.set_cursor(mondrian_ui_core::widget::CursorRequest::Grabbing);
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers }
                if self.viewport_keyboard(*key, *modifiers, ctx) =>
            {
                EventResult::Handled
            }
            UiEvent::MouseDown { position, button: MouseButton::Right, .. }
                if self.bounds.contains(*position) =>
            {
                if self.has_active_pointer_interaction() || self.gesture.is_some() {
                    return EventResult::Handled;
                }
                if let Some(index) = self.hit_point(*position)
                    && let CurvePointTarget::Keyframe(id) = self.point_metadata[index].target
                    && self.selection_menu.is_some()
                {
                    if !self.selected_keys.contains(&id) {
                        self.select(Some(index));
                    }
                    self.open_selection_menu(*position, false, ctx);
                    return EventResult::Handled;
                }
                if let Some(index) = self.hit_point(*position)
                    && let Some(items) = self
                        .point_metadata
                        .get(index)
                        .and_then(|metadata| metadata.context_menu.as_ref())
                    && items.iter().any(MenuItem::is_activatable)
                {
                    let items = items.clone();
                    self.select(Some(index));
                    let mut menu = ContextMenu::new(*position, items);
                    menu.layout(self.bounds);
                    self.context_menu = Some(menu);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseDown { position, button: MouseButton::Left, modifiers }
                if self.bounds.contains(*position) =>
            {
                self.focus_visible = false;
                ctx.tooltip.hide();
                if (self.selection_deletion
                    || self.selection_time_movement
                    || self.selection_value_movement
                    || self.selection_menu.is_some())
                    && *modifiers == Modifiers::shift()
                {
                    if !self.has_active_pointer_interaction()
                        && self.gesture.is_none()
                        && let Some(index) = self.hit_point(*position)
                    {
                        self.toggle_key_selection(index);
                        ctx.request_repaint();
                    }
                    return EventResult::Handled;
                }
                if let Some(tangent) = self.hit_tangent(*position) {
                    let index = self.keyframe_indices[&tangent.keyframe_id];
                    self.select(Some(index));
                    self.begin_drag(index, false, *position, ctx);
                    self.dragging = None;
                    self.tangent_origin = Some(self.tangents.clone());
                    self.tangent_drag = Some(TangentDrag { tangent, pointer_origin: *position });
                    self.selected_tangent = Some((tangent.keyframe_id, tangent.side));
                    ctx.request_pointer_capture(self.id);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                self.selected_tangent = None;
                if let Some(index) = self.hit_point(*position) {
                    self.select(Some(index));
                    self.begin_drag(index, false, *position, ctx);
                    ctx.request_pointer_capture(self.id);
                    return EventResult::Handled;
                }
                if self.point_insertion_limit.is_some_and(|maximum| self.points.len() >= maximum) {
                    return EventResult::Handled;
                }
                self.selected_keys.clear();
                let previous_selection = self.selected;
                if let Some(index) = self.insert_point_from_input(*position, ctx) {
                    self.begin_drag(index, true, *position, ctx);
                    if self.dragging.is_some() {
                        ctx.request_pointer_capture(self.id);
                    } else {
                        self.selected = previous_selection;
                    }
                    return EventResult::Handled;
                }
                self.selected = None;
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                if let Some(drag) = self.tangent_drag {
                    let plot = self.plot_rect();
                    let point = CurvePoint {
                        x: if plot.width > 0.0 {
                            drag.tangent.point.x
                                + ((f64::from(position.x - drag.pointer_origin.x)
                                    / f64::from(plot.width))
                                    * self.viewport.time_span)
                                    as f32
                        } else {
                            drag.tangent.point.x
                        },
                        y: if plot.height > 0.0 {
                            drag.tangent.point.y
                                - ((f64::from(position.y - drag.pointer_origin.y)
                                    / f64::from(plot.height))
                                    * 2.0
                                    * self.viewport.value_radius)
                        } else {
                            drag.tangent.point.y
                        },
                    };
                    self.move_tangent(drag.tangent, point, ctx);
                    return EventResult::Handled;
                }
                if let Some(drag) = self.dragging {
                    let plot = self.plot_rect();
                    let point = CurvePoint {
                        x: if plot.width > 0.0 {
                            drag.origin.x
                                + ((f64::from(position.x - drag.pointer_origin.x)
                                    / f64::from(plot.width))
                                    * self.viewport.time_span)
                                    as f32
                        } else {
                            drag.origin.x
                        },
                        y: if plot.height > 0.0 {
                            drag.origin.y
                                - ((f64::from(position.y - drag.pointer_origin.y)
                                    / f64::from(plot.height))
                                    * 2.0
                                    * self.viewport.value_radius)
                        } else {
                            drag.origin.y
                        },
                    };
                    self.move_point_from_input(drag.index, point, ctx);
                    return EventResult::Handled;
                }
                if let Some(help) = &self.keyboard_help {
                    if self.bounds.contains(*position) {
                        ctx.tooltip
                            .show(help.clone(), Point::new(position.x, position.y + PADDING));
                    } else {
                        ctx.tooltip.hide();
                    }
                }
                EventResult::Ignored
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. }
                if self.dragging.is_some() || self.tangent_drag.is_some() =>
            {
                self.commit_drag_edit(ctx);
                ctx.release_pointer_capture(self.id);
                EventResult::Handled
            }
            UiEvent::FocusGained { source } => {
                self.focused = true;
                self.focus_visible = source.is_focus_visible();
                if self.focus_visible
                    && let Some(help) = &self.keyboard_help
                {
                    ctx.tooltip.show(
                        help.clone(),
                        Point::new(self.bounds.x, self.bounds.y + self.bounds.height),
                    );
                }
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                let was_dragging = self.dragging.is_some() || self.tangent_drag.is_some();
                self.commit_drag_edit(ctx);
                self.focused = false;
                self.focus_visible = false;
                ctx.tooltip.hide();
                self.selected = None;
                self.selected_keys.clear();
                self.selected_tangent = None;
                if was_dragging {
                    ctx.release_pointer_capture(self.id);
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::A, modifiers }
                if (self.selection_deletion
                    || self.selection_time_movement
                    || self.selection_value_movement
                    || self.selection_menu.is_some())
                    && self.focused
                    && ctx.focus.focused_widget() == Some(self.id)
                    && (modifiers.ctrl ^ modifiers.meta)
                    && !modifiers.alt
                    && !modifiers.shift =>
            {
                if !self.has_active_pointer_interaction() && self.gesture.is_none() {
                    self.selected_keys = self
                        .point_metadata
                        .iter()
                        .filter_map(|metadata| match metadata.target {
                            CurvePointTarget::Keyframe(id) => Some(id),
                            _ => None,
                        })
                        .collect();
                    self.selected = self.point_metadata.iter().position(|metadata| {
                        matches!(metadata.target,
                        CurvePointTarget::Keyframe(id) if self.selected_keys.contains(&id))
                    });
                    self.selected_tangent = None;
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers }
                if self.navigate_keyboard(*key, *modifiers, ctx) =>
            {
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Escape, .. } if self.selected.is_some() => {
                let was_dragging = self.dragging.is_some() || self.tangent_drag.is_some();
                let changed = self.cancel_drag_state();
                self.end_gesture(true, ctx);
                if !was_dragging {
                    self.selected_keys.clear();
                    self.selected = None;
                    self.selected_tangent = None;
                }
                if changed && self.on_value_edit.is_none() {
                    self.dispatch_change(ctx);
                }
                if was_dragging {
                    ctx.release_pointer_capture(self.id);
                }
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Delete | KeyCode::Backspace, .. }
                if self.selected_tangent.is_some() =>
            {
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Delete | KeyCode::Backspace, .. }
                if self.delete_selected_from_input(ctx) =>
            {
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Up | KeyCode::Down, modifiers }
                if self.fixed_value.is_some()
                    && self.selected.is_some()
                    && !modifiers.ctrl
                    && !modifiers.alt
                    && !modifiers.meta =>
            {
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers } if self.nudge_selected(*key, *modifiers, ctx) => {
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if let Some(menu) = &self.context_menu {
            menu.paint_overlay(ctx);
        }
    }

    fn overlay_hit_test(&self, point: Point) -> bool {
        self.context_menu.as_ref().is_some_and(|menu| menu.overlay_hit_test(point))
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let v = CurveEditorVisualTokens::from_theme(ctx.theme);
        let plot = self.plot_rect();
        let grid = color_with_alpha(colors.border, v.grid_alpha);
        let curve = if self.enabled {
            color_with_alpha(colors.foreground, v.curve_alpha)
        } else {
            colors.muted_foreground
        };
        let point_fill = colors.surface;

        ctx.encoder.draw_rect(self.bounds, colors.card, spacing.radius_md);
        ctx.encoder.draw_rect(
            plot,
            color_with_alpha(colors.surface, v.plot_surface_alpha),
            spacing.radius_sm,
        );

        if self.focus_visible {
            paint_focus_ring(ctx, self.bounds, spacing.radius_md);
        }

        for col in 0..=self.grid_columns {
            let x = plot.x + plot.width * col as f32 / self.grid_columns.max(1) as f32;
            ctx.encoder.draw_line(
                Point::new(x, plot.y),
                Point::new(x, plot.y + plot.height),
                v.grid_line_width,
                grid,
            );
        }
        for row in 0..=self.grid_rows {
            let y = plot.y + plot.height * row as f32 / self.grid_rows.max(1) as f32;
            ctx.encoder.draw_line(
                Point::new(plot.x, y),
                Point::new(plot.x + plot.width, y),
                v.grid_line_width,
                grid,
            );
        }

        self.paint_rulers(ctx);
        ctx.encoder.push_clip(plot);
        for tangent in self
            .tangents
            .iter()
            .filter(|tangent| Some(tangent.keyframe_id) == self.selected_keyframe())
        {
            if let Some((owner, control)) = self.geometry().visible_segment(
                self.points[self.keyframe_indices[&tangent.keyframe_id]],
                tangent.point,
            ) {
                ctx.encoder
                    .draw_line(owner, control, v.grid_line_width, colors.muted_foreground);
            }
            let control = self.to_screen(tangent.point);
            if !plot.contains(control) {
                continue;
            }
            let radius = POINT_RADIUS;
            ctx.encoder.draw_rect(
                Rect::new(
                    control.x - radius,
                    control.y - radius,
                    2.0 * radius,
                    2.0 * radius,
                ),
                if self.selected_tangent == Some((tangent.keyframe_id, tangent.side)) {
                    colors.foreground
                } else {
                    colors.muted_foreground
                },
                radius,
            );
        }
        let painted_curve = self.display_points.as_deref().unwrap_or(&self.points);
        for pair in painted_curve.windows(2) {
            if let Some((a, b)) = self.geometry().visible_segment(pair[0], pair[1]) {
                ctx.encoder.draw_line(a, b, v.curve_line_width, curve);
            }
        }

        for (index, point) in self.points.iter().enumerate() {
            let selected = self.selected == Some(index)
                || matches!(self.point_metadata[index].target,
                CurvePointTarget::Keyframe(id) if self.selected_keys.contains(&id));
            let radius = if selected {
                SELECTED_POINT_RADIUS
            } else {
                POINT_RADIUS
            };
            let center = self.to_screen(*point);
            if !plot.contains(center) {
                continue;
            }
            let rect = Rect::new(
                center.x - radius,
                center.y - radius,
                radius * 2.0,
                radius * 2.0,
            );
            ctx.encoder.draw_rect(
                rect.inset(v.point_border_inset, v.point_border_inset),
                color_with_alpha(colors.border_strong, v.point_border_alpha),
                radius + 1.0,
            );
            ctx.encoder.draw_rect(rect, point_fill, radius);
            if selected {
                ctx.encoder.draw_rect(
                    rect.inset(v.selected_inner_inset, v.selected_inner_inset),
                    curve,
                    (radius - v.selected_inner_inset).max(0.0),
                );
            }
        }
        ctx.encoder.pop_clip();
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn has_active_pointer_interaction(&self) -> bool {
        self.dragging.is_some() || self.tangent_drag.is_some() || self.panning.is_some()
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }

    fn accessibility(&self) -> Option<AccessibilityNode> {
        let mut node = AccessibilityNode::new(self.id, AccessibilityRole::Canvas)
            .with_state(AccessibilityState {
                focusable: self.enabled,
                focused: self.focused,
                disabled: !self.enabled,
                selected: Some(self.selected.is_some()),
                expanded: Some(self.context_menu.is_some()),
                ..AccessibilityState::default()
            })
            .with_value(AccessibilityValue::Collection {
                total_count: self.points.len(),
                visible_count: self
                    .points
                    .iter()
                    .filter(|point| self.plot_rect().contains(self.to_screen(**point)))
                    .count(),
                selected_count: self.selected_keys.len().max(usize::from(self.selected.is_some())),
            });
        node.name = self.accessible_name.clone();
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_core::Color;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        lines: usize,
        texts: Vec<(String, f32, Point)>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rects.push(bounds);
        }

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
        }

        fn draw_text(&mut self, text: &str, font_size: f32, position: Point, _color: Color) {
            self.texts.push((text.into(), font_size, position));
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn event_ctx() -> EventContext<'static> {
        let f: &'static mut DummyFocus = Box::leak(Box::new(DummyFocus));
        let s: &'static mut DummyShortcut = Box::leak(Box::new(DummyShortcut));
        let t: &'static mut DummyTooltip = Box::leak(Box::new(DummyTooltip));
        make_event_ctx(f, s, t, &|_| {})
    }

    fn curve_action(points: &[CurvePoint]) -> Action {
        let mut name = String::from("points");
        for point in points {
            name.push_str(&format!(":{:.2},{:.2}", point.x, point.y));
        }
        Action::Custom {
            namespace: "test.curve".into(),
            name,
            payload: Default::default(),
        }
    }

    #[test]
    fn authored_multiselection_is_stable_excludes_boundaries_and_deletes_once() {
        let ids = [
            mondrian_core::KeyframeId::new(),
            mondrian_core::KeyframeId::new(),
        ];
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.25, 0.2),
            CurvePoint::new(0.75, 0.8),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_keyframe_ids(vec![None, Some(ids[0]), Some(ids[1]), None])
        .with_selection_deletion()
        .with_view_key(Some("selection-owner".into()));
        editor.layout(Rect::new(0.0, 0.0, 400.0, 200.0));
        let emitted = events.clone();
        editor = editor.on_edit(move |edit| {
            emitted.borrow_mut().push(edit);
            None
        });
        let mut ctx = event_ctx();
        for index in [1, 2, 0, 3] {
            editor.event(
                &UiEvent::MouseDown {
                    position: editor.point_position(editor.points()[index]),
                    button: MouseButton::Left,
                    modifiers: Modifiers::shift(),
                },
                &mut ctx,
            );
        }
        assert_eq!(editor.selected_keyframe_ids(), ids);
        assert!(events.borrow().is_empty());
        assert!(!editor.has_active_pointer_interaction());
        let snapshot = editor.view_state().expect("inactive selected view");
        let mut replacement = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.1, 0.8),
            CurvePoint::new(0.9, 0.2),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_keyframe_ids(vec![None, Some(ids[1]), Some(ids[0]), None])
        .with_selection_deletion()
        .with_view_key(Some("selection-owner".into()));
        assert!(replacement.restore_view_state(&snapshot));
        assert_eq!(replacement.selected_keyframe_ids(), [ids[1], ids[0]]);
        let mut shrunk = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.2),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_keyframe_ids(vec![None, Some(ids[0]), None])
        .with_selection_deletion()
        .with_view_key(Some("selection-owner".into()));
        assert!(shrunk.restore_view_state(&snapshot));
        assert_eq!(shrunk.selected_keyframe_ids(), [ids[0]]);
        assert_eq!(
            shrunk.selected_index(),
            Some(1),
            "surviving key replaces removed primary"
        );
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            *events.borrow(),
            [CurveEdit::DeleteMany { keyframe_ids: ids.to_vec() }]
        );
        assert_eq!(editor.points().len(), 2);
        assert!(editor.selected_keyframe_ids().is_empty());
    }

    #[test]
    fn selected_key_time_keyboard_dispatch_is_exact_and_requires_group_admission() {
        let ids = [
            mondrian_core::KeyframeId::new(),
            mondrian_core::KeyframeId::new(),
        ];
        let points = vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.25, 0.2),
            CurvePoint::new(0.75, 0.8),
            CurvePoint::new(1.0, 1.0),
        ];
        let frame = mondrian_core::TimelineTime::new(1001, 24000).expect("frame grid");
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let emitted = events.clone();
        let mut editor = CurveEditor::with_points(points.clone())
            .with_keyframe_ids(vec![None, Some(ids[0]), Some(ids[1]), None])
            .with_selection_time_movement()
            .with_frame_time_step(frame)
            .with_view_key(Some("group-time".into()))
            .on_edit(move |edit| {
                emitted.borrow_mut().push(edit);
                None
            });
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &|_| {});
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        editor.point_metadata[1].policy.deletable = false;
        editor.layout(Rect::new(0.0, 0.0, 400.0, 200.0));
        for index in [1, 2] {
            editor.event(
                &UiEvent::MouseDown {
                    position: editor.point_position(editor.points()[index]),
                    button: MouseButton::Left,
                    modifiers: Modifiers::shift(),
                },
                &mut ctx,
            );
        }
        assert_eq!(editor.selected_keyframe_ids(), ids);
        assert!(!editor.has_active_pointer_interaction());
        editor.select(None);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        assert_eq!(editor.selected_keyframe_ids(), ids);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Left, modifiers: Modifiers::shift() },
            &mut ctx,
        );
        assert_eq!(
            *events.borrow(),
            [
                CurveEdit::NudgeTimeMany { keyframe_ids: ids.to_vec(), delta: frame },
                CurveEdit::NudgeTimeMany {
                    keyframe_ids: ids.to_vec(),
                    delta: frame
                        .checked_scale(mondrian_core::TimeScale::new(-5, 1).expect("five frames"))
                        .expect("delta")
                },
            ]
        );
        assert_eq!(editor.points(), points);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            events.borrow().len(),
            2,
            "movement cannot admit group deletion"
        );
        assert_eq!(editor.points(), points);
        assert_eq!(editor.selected_keyframe_ids(), ids);
        let snapshot = editor.view_state().expect("selected inactive view");
        let mut revoked = CurveEditor::with_points(points.clone())
            .with_keyframe_ids(vec![None, Some(ids[0]), Some(ids[1]), None])
            .with_selection_deletion()
            .with_frame_time_step(frame)
            .with_view_key(Some("group-time".into()));
        assert!(revoked.restore_view_state(&snapshot));
        let before = events.borrow().len();
        let emitted = events.clone();
        revoked = revoked.on_edit(move |edit| {
            emitted.borrow_mut().push(edit);
            None
        });
        ctx.focus.request_focus(revoked.id());
        revoked.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            events.borrow().len(),
            before,
            "capability loss cannot nudge only the primary"
        );
        ctx.focus.request_focus(editor.id());
        editor.point_metadata[1].policy.lock_x = true;
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            events.borrow().len(),
            before,
            "one locked key rejects the complete set"
        );
        assert_eq!(editor.selected_keyframe_ids(), ids);
    }

    #[test]
    fn replacing_authored_geometry_clears_the_entire_stable_selection() {
        let ids = [
            mondrian_core::KeyframeId::new(),
            mondrian_core::KeyframeId::new(),
        ];
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.25, 0.2),
            CurvePoint::new(0.75, 0.8),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_keyframe_ids(vec![None, Some(ids[0]), Some(ids[1]), None])
        .with_selection_deletion();
        editor.toggle_key_selection(1);
        editor.toggle_key_selection(2);
        assert_eq!(editor.selected_keyframe_ids(), ids);
        editor.set_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        assert!(
            editor.selected_keys.is_empty(),
            "replacement has no author identities"
        );
        assert_eq!(
            editor.selected_index(),
            None,
            "old author selection cannot become a snapshot point"
        );
        let mut ctx = event_ctx();
        assert!(!editor.delete_selected_from_input(&mut ctx));
        assert_eq!(editor.points().len(), 3);
    }

    #[test]
    fn refreshed_multiselection_without_batch_admission_never_deletes_only_primary() {
        let ids = [
            mondrian_core::KeyframeId::new(),
            mondrian_core::KeyframeId::new(),
        ];
        let points = vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.25, 0.2),
            CurvePoint::new(0.75, 0.8),
            CurvePoint::new(1.0, 1.0),
        ];
        let mut original = CurveEditor::with_points(points.clone())
            .with_keyframe_ids(vec![None, Some(ids[0]), Some(ids[1]), None])
            .with_selection_deletion()
            .with_view_key(Some("same-author-owner".into()));
        original.toggle_key_selection(1);
        original.toggle_key_selection(2);
        let state = original.view_state().expect("inactive selected view");
        let actions = std::rc::Rc::new(RefCell::new(Vec::new()));
        let emitted = actions.clone();
        let mut replacement = CurveEditor::with_points(points.clone())
            .with_keyframe_ids(vec![None, Some(ids[0]), Some(ids[1]), None])
            .with_view_key(Some("same-author-owner".into()))
            .on_edit(move |edit| {
                emitted.borrow_mut().push(edit);
                None
            });
        assert!(replacement.restore_view_state(&state));
        assert_eq!(replacement.selected_keyframe_ids(), ids);
        let mut ctx = event_ctx();
        replacement.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(
            actions.borrow().is_empty(),
            "unadmitted set must not emit a singleton edit"
        );
        assert_eq!(replacement.points(), points);
        assert_eq!(replacement.selected_keyframe_ids(), ids);
    }

    #[test]
    fn rejected_selection_delete_restores_geometry_and_entire_selection() {
        let ids = [
            mondrian_core::KeyframeId::new(),
            mondrian_core::KeyframeId::new(),
        ];
        let original = vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.25, 0.2),
            CurvePoint::new(0.75, 0.8),
            CurvePoint::new(1.0, 1.0),
        ];
        let mut editor = CurveEditor::with_points(original.clone())
            .with_keyframe_ids(vec![None, Some(ids[0]), Some(ids[1]), None])
            .with_selection_deletion()
            .with_preview_projection(|_| None);
        editor.toggle_key_selection(1);
        editor.toggle_key_selection(2);
        let selected = editor.selected_index();
        let mut ctx = event_ctx();
        assert!(editor.delete_selected_from_input(&mut ctx));
        assert_eq!(editor.points(), original);
        assert_eq!(editor.selected_keyframe_ids(), ids);
        assert_eq!(editor.selected_index(), selected);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(editor.selected_keyframe_ids().is_empty());
        assert_eq!(editor.points(), original);
    }

    #[test]
    fn view_state_restores_stable_key_and_focus_after_reordering() {
        let id = mondrian_core::KeyframeId::new();
        let mut original = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_keyframe_ids(vec![None, Some(id), None])
        .with_view_key(Some("owner".into()));
        original.select(Some(1));
        original.focused = true;
        original.focus_visible = true;
        let state = original.view_state().expect("inactive state");
        assert!(
            state.points.is_none(),
            "author IDs require no geometry snapshot"
        );
        let mut refreshed = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.25, 0.3),
            CurvePoint::new(0.6, 0.7),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_keyframe_ids(vec![
            None,
            Some(mondrian_core::KeyframeId::new()),
            Some(id),
            None,
        ])
        .with_view_key(Some("owner".into()));
        assert!(refreshed.restore_view_state(&state));
        assert_eq!(refreshed.id(), original.id());
        assert_eq!(refreshed.selected_index(), Some(2));
        assert!(refreshed.focused && refreshed.focus_visible);
        assert_eq!(
            refreshed.points()[2],
            CurvePoint::new(0.6, 0.7),
            "new canonical geometry remains authoritative"
        );
    }

    #[test]
    fn view_state_does_not_transfer_to_another_owner_or_deleted_key() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut original = tangent_editor(events).with_view_key(Some("owner".into()));
        original.selected_tangent =
            Some((original.tangents[0].keyframe_id, original.tangents[0].side));
        let state = original.view_state().expect("state");
        let mut other = CurveEditor::new().with_view_key(Some("other".into()));
        let other_id = other.id();
        assert!(!other.restore_view_state(&state));
        assert_eq!(other.id(), other_id);
        let mut replacement = CurveEditor::new().with_view_key(Some("owner".into()));
        assert!(replacement.restore_view_state(&state));
        assert!(replacement.selected_index().is_none());
        assert!(replacement.selected_tangent.is_none());
    }

    #[test]
    fn view_state_snapshot_selection_requires_identical_geometry() {
        let mut original = CurveEditor::new().with_view_key(Some("transfer".into()));
        original.select(Some(1));
        let state = original.view_state().expect("state");
        let mut same = CurveEditor::with_points(original.points().to_vec())
            .with_view_key(Some("transfer".into()));
        assert!(same.restore_view_state(&state));
        assert_eq!(same.selected_index(), Some(1));
        let mut changed = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.8),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_view_key(Some("transfer".into()));
        assert!(changed.restore_view_state(&state));
        assert!(changed.selected_index().is_none());
    }

    struct CurveFocus(Option<WidgetId>);
    impl mondrian_ui_core::focus::FocusManager for CurveFocus {
        fn focused_widget(&self) -> Option<WidgetId> {
            self.0
        }
        fn focused_panel(&self) -> Option<mondrian_editor_state::state::PanelKind> {
            None
        }
        fn request_focus(&mut self, id: WidgetId) {
            self.0 = Some(id);
        }
        fn release_focus(&mut self, id: WidgetId) {
            if self.0 == Some(id) {
                self.0 = None;
            }
        }
        fn clear_focus(&mut self) {
            self.0 = None;
        }
    }

    #[test]
    fn keyboard_selection_requires_focus_and_never_authors_points() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut editor = CurveEditor::new().on_change(curve_action);
        let original = editor.points().to_vec();
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        assert_eq!(
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
                &mut ctx
            ),
            EventResult::Ignored
        );
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        for (key, modifiers, index) in [
            (KeyCode::Home, Modifiers::none(), 0),
            (KeyCode::Right, Modifiers::ctrl(), 1),
            (
                KeyCode::Right,
                Modifiers { meta: true, ..Modifiers::none() },
                2,
            ),
            (KeyCode::End, Modifiers::none(), 3),
            (KeyCode::Right, Modifiers::ctrl(), 3),
            (KeyCode::Left, Modifiers::ctrl(), 2),
        ] {
            assert_eq!(
                editor.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Handled
            );
            assert_eq!(editor.selected_index(), Some(index));
        }
        assert_eq!(
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
                &mut ctx
            ),
            EventResult::Ignored
        );
        assert_eq!(editor.points(), original);
        assert!(actions.borrow().is_empty());
        assert!(editor.focus_visible);
    }

    #[test]
    fn keyboard_selects_both_handles_and_returns_to_owner_without_authoring() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut editor = tangent_editor(events.clone());
        let id = editor.tangents[0].keyframe_id;
        let mut tangents = editor.tangents.clone();
        tangents.push(CurveTangent {
            keyframe_id: id,
            side: mondrian_core::BezierHandleSide::Incoming,
            point: CurvePoint::new(0.25, 0.5),
            time_limits: [0.0, 0.5],
        });
        editor.set_tangents(tangents);
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &|_| {});
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let alt = Modifiers { alt: true, ..Modifiers::none() };
        for (key, side) in [
            (KeyCode::Right, mondrian_core::BezierHandleSide::Outgoing),
            (KeyCode::Left, mondrian_core::BezierHandleSide::Incoming),
        ] {
            assert_eq!(
                editor.event(&UiEvent::KeyDown { key, modifiers: alt }, &mut ctx),
                EventResult::Handled
            );
            assert_eq!(editor.selected_tangent, Some((id, side)));
        }
        assert_eq!(
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Up, modifiers: alt },
                &mut ctx
            ),
            EventResult::Handled
        );
        assert!(editor.selected_tangent.is_none());
        assert_eq!(editor.selected_keyframe(), Some(id));
        assert!(events.borrow().is_empty());
    }

    #[test]
    fn keyboard_navigation_does_not_switch_captured_handle_owner() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut editor = tangent_editor(events.clone());
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &|_| {});
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        grab_tangent(&mut editor, &mut ctx);
        let selection = (editor.selected, editor.selected_tangent);
        let count = events.borrow().len();
        for (key, modifiers) in [
            (KeyCode::Home, Modifiers::none()),
            (KeyCode::Left, Modifiers::ctrl()),
            (KeyCode::Up, Modifiers { alt: true, ..Modifiers::none() }),
            (KeyCode::F10, Modifiers::shift()),
        ] {
            assert_eq!(
                editor.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Handled
            );
            assert_eq!((editor.selected, editor.selected_tangent), selection);
            assert!(editor.has_active_pointer_interaction());
            assert!(editor.context_menu.is_none());
        }
        assert_eq!(events.borrow().len(), count);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(!editor.has_active_pointer_interaction());
        assert!(matches!(
            events.borrow().last(),
            Some(CurveGestureEvent::Cancel(_))
        ));
    }

    #[test]
    fn keyboard_context_menu_activates_the_selected_points_command() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut editor = CurveEditor::new()
            .with_point_context_menus(vec![
                None,
                Some(vec![MenuItem::new("Auto", Action::Paste)]),
                None,
                None,
            ])
            .with_keyboard_help("Curve", "Keyboard help");
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        editor.select(Some(1));
        assert_eq!(
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::F10, modifiers: Modifiers::shift() },
                &mut ctx
            ),
            EventResult::Handled
        );
        let node = editor.accessibility().expect("canvas accessibility");
        assert_eq!(node.role, AccessibilityRole::Canvas);
        assert_eq!(node.name.as_deref(), Some("Curve"));
        assert_eq!(node.state.expanded, Some(true));
        assert!(node.state.focused);
        assert_eq!(
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx
            ),
            EventResult::Handled
        );
        assert_eq!(actions.borrow().as_slice(), &[Action::Paste]);
    }

    #[test]
    fn selected_value_keyboard_has_independent_capability_and_whole_set_lock_admission() {
        let ids = [
            mondrian_core::KeyframeId::new(),
            mondrian_core::KeyframeId::new(),
        ];
        let range = mondrian_core::ParameterNumericRange::new(0.0, 1.0).expect("range");
        let axis = CurveValueAxis::new(range, range, Some(0.125)).expect("axis");
        let points = vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.25, 0.25),
            CurvePoint::new(0.75, 0.75),
            CurvePoint::new(1.0, 1.0),
        ];
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let emitted = events.clone();
        let mut editor = CurveEditor::with_author_points(points.clone(), axis)
            .with_keyframe_ids(vec![None, Some(ids[0]), Some(ids[1]), None])
            .with_selection_value_movement()
            .on_edit(move |edit| {
                emitted.borrow_mut().push(edit);
                None
            });
        editor.layout(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &|_| {});
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        assert_eq!(editor.selected_keyframe_ids(), ids);
        editor.point_metadata[1].policy.deletable = false;
        for (key, modifiers) in [
            (KeyCode::Down, Modifiers::none()),
            (KeyCode::Up, Modifiers::shift()),
        ] {
            editor.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx);
        }
        assert_eq!(
            *events.borrow(),
            [
                CurveEdit::NudgeValueMany {
                    keyframe_ids: ids.to_vec(),
                    delta: 0.125,
                    steps: -1
                },
                CurveEdit::NudgeValueMany { keyframe_ids: ids.to_vec(), delta: 0.125, steps: 5 },
            ]
        );
        assert_eq!(editor.points(), points);
        for key in [KeyCode::Delete, KeyCode::Right] {
            editor.event(
                &UiEvent::KeyDown { key, modifiers: Modifiers::none() },
                &mut ctx,
            );
        }
        assert_eq!(
            events.borrow().len(),
            2,
            "value permission cannot admit other group operations"
        );
        editor.point_metadata[2].policy.lock_y = true;
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            events.borrow().len(),
            2,
            "one locked key refuses the whole set"
        );
        editor.point_metadata[2].policy.lock_y = false;
        editor.selection_value_movement = false;
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            events.borrow().len(),
            2,
            "retained selection cannot bypass revoked capability"
        );
        assert_eq!(editor.selected_keyframe_ids(), ids);
        assert_eq!(editor.points(), points);
    }

    #[test]
    fn value_lock_constrains_pointer_coordinates_and_singleton_increment() {
        let id = mondrian_core::KeyframeId::new();
        let range = mondrian_core::ParameterNumericRange::new(0.0, 1.0).expect("range");
        let axis = CurveValueAxis::new(range, range, Some(0.125)).expect("axis");
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let emitted = events.clone();
        let mut editor = CurveEditor::with_author_points(
            vec![
                CurvePoint::new(0.0, 0.0),
                CurvePoint::new(0.5, 0.5),
                CurvePoint::new(1.0, 1.0),
            ],
            axis,
        )
        .with_keyframe_ids(vec![None, Some(id), None])
        .on_edit(move |edit| {
            emitted.borrow_mut().push(edit);
            None
        });
        editor.point_metadata[1].policy.lock_y = true;
        assert!(editor.move_point(1, CurvePoint::new(0.75, 0.9)));
        assert_eq!(
            editor.points()[1],
            CurvePoint::new(0.75, 0.5),
            "pointer constraint retains only locked coordinate"
        );
        editor.layout(Rect::new(0.0, 0.0, 400.0, 200.0));
        editor.select(Some(1));
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &|_| {});
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(events.borrow().is_empty());
        editor.point_metadata[1].policy.lock_y = false;
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            *events.borrow(),
            [CurveEdit::NudgeValue { keyframe_id: id, delta: 0.125, steps: 1 }]
        );
        assert_eq!(
            editor.points()[1],
            CurvePoint::new(0.75, 0.5),
            "canonical preview owns value changes"
        );
    }

    fn curve_edit_action(edit: CurveEdit) -> Action {
        let name = match edit {
            CurveEdit::NudgeValue { keyframe_id, delta, steps } => {
                format!("value:{keyframe_id}:{delta}:{steps}")
            }
            CurveEdit::NudgeValueMany { keyframe_ids, delta, steps } => {
                format!("value-many:{keyframe_ids:?}:{delta}:{steps}")
            }
            CurveEdit::NudgeTime { keyframe_id, side, delta } => {
                format!("time:{keyframe_id}:{side:?}:{delta:?}")
            }
            CurveEdit::Insert { index, point, .. } => {
                format!("insert:{index}:{:.4},{:.4}", point.x, point.y)
            }
            CurveEdit::Move { target, point } => {
                format!("move:{target:?}:{:.4},{:.4}", point.x, point.y)
            }
            CurveEdit::Handle { keyframe_id, side, .. } => format!("handle:{keyframe_id}:{side:?}"),
            CurveEdit::Delete { target } => format!("delete:{target:?}"),
            CurveEdit::NudgeTimeMany { keyframe_ids, delta } => {
                format!("nudge-time-many:{keyframe_ids:?}:{delta}")
            }
            CurveEdit::DeleteMany { keyframe_ids } => format!("delete-many:{keyframe_ids:?}"),
        };
        Action::Custom {
            namespace: "test.curve-edit".into(),
            name,
            payload: Default::default(),
        }
    }

    fn tangent_editor(events: std::rc::Rc<RefCell<Vec<CurveGestureEvent>>>) -> CurveEditor {
        let keyframe_id = mondrian_core::KeyframeId::new();
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.5),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 0.5),
        ])
        .with_keyframe_ids(vec![None, Some(keyframe_id), None])
        .with_tangents(vec![CurveTangent {
            keyframe_id,
            side: mondrian_core::BezierHandleSide::Outgoing,
            point: CurvePoint::new(0.75, 0.5),
            time_limits: [0.5, 1.0],
        }])
        .on_gesture(move |event| {
            events.borrow_mut().push(event);
            None::<Action>
        });
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        editor.select(Some(1));
        editor
    }

    #[test]
    fn viewport_pan_cancels_and_restores_across_owner_refresh_without_edits() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut editor = CurveEditor::new()
            .with_view_key(Some("owner".into()))
            .on_edit(curve_edit_action);
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let zoom = UiEvent::MouseWheel {
            position: Point::new(110.0, 52.0),
            delta: -100.0,
            modifiers: Modifiers::ctrl(),
        };
        editor.event(&zoom, &mut ctx);
        let origin = editor.viewport;
        let down = UiEvent::MouseDown {
            position: Point::new(110.0, 52.0),
            button: MouseButton::Middle,
            modifiers: Modifiers::none(),
        };
        editor.event(&down, &mut ctx);
        assert!(editor.has_active_pointer_interaction());
        assert!(editor.view_state().is_none());
        editor.event(
            &UiEvent::MouseMove {
                position: Point::new(140.0, 62.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(editor.viewport.time_start < origin.time_start);
        assert!(editor.viewport.value_center > origin.value_center);
        let moved = editor.viewport;
        editor.event(&zoom, &mut ctx);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.viewport, moved);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.viewport, origin);
        assert!(!editor.has_active_pointer_interaction());
        editor.event(&down, &mut ctx);
        editor.event(
            &UiEvent::MouseMove {
                position: Point::new(140.0, 62.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseUp {
                position: Point::new(140.0, 62.0),
                button: MouseButton::Middle,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!editor.has_active_pointer_interaction());
        let state = editor.view_state().expect("inactive view");
        let mut refreshed = CurveEditor::new().with_view_key(Some("owner".into()));
        refreshed.layout(Rect::new(20.0, 30.0, 440.0, 208.0));
        assert!(refreshed.restore_view_state(&state));
        assert_eq!(refreshed.viewport, editor.viewport);
        let mut other = CurveEditor::new().with_view_key(Some("other".into()));
        assert!(!other.restore_view_state(&state));
        assert_eq!(other.viewport, CurveViewport::default());
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Digit0, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        assert_eq!(editor.viewport, CurveViewport::default());
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn viewport_pan_termination_releases_only_owned_capture_and_emits_no_author_events() {
        for termination in 0..3 {
            let events = std::rc::Rc::new(RefCell::new(Vec::new()));
            let mut editor = tangent_editor(events.clone());
            let mut focus = CurveFocus(Some(editor.id()));
            let mut shortcuts = DummyShortcut;
            let mut tooltip = DummyTooltip;
            let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &|_| {});
            editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
            let origin = editor.viewport;
            editor.event(
                &UiEvent::MouseDown {
                    position: Point::new(110.0, 52.0),
                    button: MouseButton::Middle,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            editor.event(
                &UiEvent::MouseMove {
                    position: Point::new(130.0, 62.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            match termination {
                0 => {
                    editor.event(&UiEvent::FocusLost, &mut ctx);
                }
                1 => {
                    editor.set_enabled(false);
                    editor.event(
                        &UiEvent::MouseMove {
                            position: Point::new(130.0, 62.0),
                            modifiers: Modifiers::none(),
                        },
                        &mut ctx,
                    );
                    assert_eq!(editor.viewport, origin);
                }
                _ => {
                    editor.set_points(editor.points.clone());
                    editor.event(
                        &UiEvent::MouseMove {
                            position: Point::new(130.0, 62.0),
                            modifiers: Modifiers::none(),
                        },
                        &mut ctx,
                    );
                    assert_eq!(editor.viewport, origin);
                }
            }
            assert!(!editor.has_active_pointer_interaction());
            assert_eq!(
                ctx.requests.pointer_capture,
                Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                    editor.id()
                ))
            );
            assert_eq!(
                ctx.requests.cursor,
                Some(mondrian_ui_core::widget::CursorRequest::Default)
            );
            assert!(
                events.borrow().is_empty(),
                "view capture never opens an author gesture"
            );
        }
    }

    #[test]
    fn value_zoom_scales_handle_drag_and_leaves_time_exactly_unchanged() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut editor = tangent_editor(events.clone());
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &|_| {});
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        editor.event(
            &UiEvent::MouseWheel {
                position: Point::new(110.0, 52.0),
                delta: -100.0,
                modifiers: Modifiers { shift: true, ..Modifiers::ctrl() },
            },
            &mut ctx,
        );
        assert_eq!(editor.viewport.time_span, 1.0);
        let original = editor.tangents[0].point;
        let span = editor.viewport.value_radius * 2.0;
        let position = grab_tangent(&mut editor, &mut ctx);
        let viewport = editor.viewport;
        editor.event(
            &UiEvent::MouseWheel {
                position,
                delta: -100.0,
                modifiers: Modifiers::ctrl(),
            },
            &mut ctx,
        );
        assert_eq!(editor.viewport, viewport, "draft projection remains stable");
        editor.event(
            &UiEvent::MouseMove {
                position: Point::new(position.x, position.y - 8.4),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(editor.tangents[0].point.x, original.x);
        assert!(((editor.tangents[0].point.y - original.y) - 0.1 * span).abs() < 0.000001);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.tangents[0].point, original);
    }

    #[test]
    fn wheel_zoom_keeps_pointer_author_coordinate_and_creates_no_edit() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_edit(curve_edit_action);
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let original = editor.points.clone();
        let anchor = CurvePoint::new(0.25, 0.75);
        let pointer = editor.to_screen(anchor);
        let before = editor.to_screen(original[1]);
        assert_eq!(
            editor.event(
                &UiEvent::MouseWheel {
                    position: pointer,
                    delta: -20.0,
                    modifiers: Modifiers::ctrl()
                },
                &mut ctx
            ),
            EventResult::Handled
        );
        let after = editor.to_screen(original[1]);
        assert!(after.x > before.x, "time zoom magnifies around pointer");
        assert_eq!(after.y, before.y, "time zoom preserves the value axis");
        assert!((editor.to_screen(anchor).x - pointer.x).abs() < 0.0001);
        assert_eq!(editor.points, original);
        assert!(
            actions.borrow().is_empty(),
            "viewport is not author history"
        );
    }

    #[test]
    fn temporal_value_keyboard_emits_an_edit_above_f64_integer_precision() {
        use mondrian_core::automation::ParameterNumericRange;
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let id = mondrian_core::KeyframeId::new();
        let range = ParameterNumericRange::new(i64::MIN as f64, i64::MAX as f64).expect("range");
        let axis = CurveValueAxis::new(range, range, Some(1.0)).expect("axis");
        let original = vec![
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 0.5, y: i64::MAX as f64 },
            CurvePoint { x: 1.0, y: 0.0 },
        ];
        let recorded = events.clone();
        let mut editor = CurveEditor::with_author_points(original.clone(), axis)
            .with_keyframe_ids(vec![None, Some(id), None])
            .on_gesture(move |event| {
                recorded.borrow_mut().push(event);
                None::<Action>
            });
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        editor.selected = Some(1);
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &|_| {});
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            events.borrow().len(),
            1,
            "a legal decrement must reach the canonical author value"
        );
        assert_eq!(editor.points, original, "canonical preview owns geometry");
        assert!(
            matches!(events.borrow()[0], CurveGestureEvent::Edit { gesture_id: None,
            edit: CurveEdit::NudgeValue { keyframe_id, delta: 1.0, steps: -1 } } if keyframe_id == id)
        );
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
            &mut ctx,
        );
        assert!(matches!(
            events.borrow()[1],
            CurveGestureEvent::Edit {
                gesture_id: None,
                edit: CurveEdit::NudgeValue { delta: 1.0, steps: 5, .. }
            }
        ));
        assert_eq!(editor.points, original);
    }

    #[test]
    fn exact_frame_commands_preserve_geometry_and_ignore_capture_or_invalid_grid() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let frame = mondrian_core::TimelineTime::new(1001, 30000).expect("frame");
        let mut editor = tangent_editor(events.clone()).with_frame_time_step(frame);
        let original = editor.points.clone();
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &|_| {});
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        for (key, modifiers, count) in [
            (KeyCode::Right, Modifiers::none(), 1),
            (KeyCode::Left, Modifiers::shift(), -5),
        ] {
            editor.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx);
            assert!(
                matches!(events.borrow().last(), Some(CurveGestureEvent::Edit { gesture_id: None, edit: CurveEdit::NudgeTime { side: None, delta, .. } }) if *delta == frame.checked_scale(mondrian_core::TimeScale::new(count, 1).expect("steps")).expect("delta"))
            );
            assert_eq!(
                editor.points, original,
                "author preview owns marker changes"
            );
        }
        grab_tangent(&mut editor, &mut ctx);
        let count = events.borrow().len();
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(events.borrow().len(), count, "capture owns the draft");
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        editor.frame_time_step = Some(mondrian_core::TimelineTime::ZERO);
        let count = events.borrow().len();
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            events.borrow().len(),
            count,
            "invalid grid has no normalized fallback"
        );
    }

    fn grab_tangent(editor: &mut CurveEditor, ctx: &mut EventContext) -> Point {
        let point = editor.to_screen(editor.tangents[0].point);
        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none()
                },
                ctx
            ),
            EventResult::Handled
        );
        point
    }

    #[test]
    fn temporal_handle_selection_does_not_survive_selecting_another_key() {
        for selection in [None, Some(0)] {
            let events = std::rc::Rc::new(RefCell::new(Vec::new()));
            let mut editor = tangent_editor(events.clone());
            let mut ctx = event_ctx();
            let start = grab_tangent(&mut editor, &mut ctx);
            editor.event(
                &UiEvent::MouseUp {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            editor.select(selection);
            assert_eq!(editor.selected_tangent, None);
        }
    }

    #[test]
    fn temporal_handle_collapsed_onto_owner_keeps_key_drag_accessible() {
        let mut editor = tangent_editor(std::rc::Rc::new(RefCell::new(Vec::new())));
        editor.tangents[0].point = editor.points[1];
        let position = editor.to_screen(editor.points[1]);
        editor.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut event_ctx(),
        );
        assert!(editor.dragging.is_some());
        assert!(editor.tangent_drag.is_none());
    }

    #[test]
    fn temporal_handle_keyboard_edits_are_atomic_and_never_delete_the_owner() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut editor = tangent_editor(events.clone());
        let mut ctx = event_ctx();
        let start = grab_tangent(&mut editor, &mut ctx);
        editor.event(
            &UiEvent::MouseUp {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        events.borrow_mut().clear();
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(matches!(
            events.borrow().as_slice(),
            [CurveGestureEvent::Edit { gesture_id: None, edit: CurveEdit::Handle { keyframe_id, .. } }]
                if editor.point_target(1) == Some(CurvePointTarget::Keyframe(*keyframe_id))
        ));
        let count = editor.points().len();
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.points().len(), count);
        assert_eq!(events.borrow().len(), 1);
    }

    #[test]
    fn temporal_handle_on_edit_commits_once_after_pointer_release() {
        let edits = std::rc::Rc::new(RefCell::new(Vec::new()));
        let recorded = edits.clone();
        let mut editor =
            tangent_editor(std::rc::Rc::new(RefCell::new(Vec::new()))).on_edit(move |edit| {
                recorded.borrow_mut().push(edit);
                None::<Action>
            });
        let mut ctx = event_ctx();
        let start = grab_tangent(&mut editor, &mut ctx);
        for offset in [10.0, 20.0] {
            editor.event(
                &UiEvent::MouseMove {
                    position: Point::new(start.x, start.y - offset),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
        }
        assert!(edits.borrow().is_empty());
        editor.event(
            &UiEvent::MouseUp {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(matches!(
            edits.borrow().as_slice(),
            [CurveEdit::Handle { keyframe_id, .. }]
                if editor.point_target(1) == Some(CurvePointTarget::Keyframe(*keyframe_id))
        ));
    }

    #[test]
    fn deleting_a_key_keeps_the_shifted_keys_stable_author_target() {
        let removed = mondrian_core::KeyframeId::new();
        let survivor = mondrian_core::KeyframeId::new();
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let mut editor = CurveEditor::new()
            .with_keyframe_ids(vec![None, Some(removed), Some(survivor), None])
            .on_gesture(move |event| {
                recorded.borrow_mut().push(event);
                None::<Action>
            });
        editor.select(Some(1));
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut event_ctx(),
        );
        assert_eq!(
            editor.point_target(1),
            Some(CurvePointTarget::Keyframe(survivor))
        );
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut event_ctx(),
        );
        assert!(matches!(events.borrow().as_slice(), [
            CurveGestureEvent::Edit { gesture_id: None, edit: CurveEdit::Delete { target: CurvePointTarget::Keyframe(id) } },
            CurveGestureEvent::Edit { gesture_id: None, edit: CurveEdit::Move { target: CurvePointTarget::Keyframe(next), .. } },
        ] if *id == removed && *next == survivor));
    }

    #[test]
    fn deleting_a_generic_point_preserves_its_neighbors_snapshot_identity() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let mut editor = CurveEditor::new().on_edit(move |edit| {
            recorded.borrow_mut().push(edit);
            None::<Action>
        });
        editor.select(Some(1));
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut event_ctx(),
        );
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut event_ctx(),
        );
        assert!(matches!(
            events.borrow().as_slice(),
            [
                CurveEdit::Delete { target: CurvePointTarget::Snapshot(1) },
                CurveEdit::Move { target: CurvePointTarget::Snapshot(2), .. },
            ]
        ));
    }

    #[test]
    fn insertion_cancel_restores_key_identity_menu_and_canonical_controls() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut editor = tangent_editor(events.clone());
        let owner = editor.point_target(1);
        let points = editor.points.clone();
        let original = editor.tangents.clone();
        let projection = original.clone();
        editor = editor
            .with_point_context_menus(vec![
                None,
                Some(vec![MenuItem::new("Owner menu", curve_action(&points))]),
                None,
            ])
            .with_preview_projection(move |event| {
                matches!(event, CurveGestureEvent::Edit { .. }).then(|| CurvePreview {
                    virtual_boundaries: [None, None],
                    key_points: Vec::new(),
                    display_points: vec![CurvePoint::new(0.0, 0.5), CurvePoint::new(1.0, 0.5)],
                    tangents: projection
                        .iter()
                        .map(|tangent| CurveTangent {
                            point: CurvePoint { x: 0.6, ..tangent.point },
                            ..*tangent
                        })
                        .collect(),
                })
            });
        let insert = editor.to_screen(CurvePoint::new(0.25, 0.75));
        let mut ctx = event_ctx();
        editor.event(
            &UiEvent::MouseDown {
                position: insert,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(editor.point_target(2), owner);
        assert!(editor.point_metadata[1].context_menu.is_none());
        assert!(editor.point_metadata[2].context_menu.is_some());
        assert_eq!(
            editor.hit_tangent(editor.to_screen(editor.tangents[0].point)),
            None,
            "a new key cannot claim controls belonging to the displaced owner"
        );
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.points, points);
        assert_eq!(editor.point_target(1), owner);
        assert!(editor.point_metadata[1].context_menu.is_some());
        assert_eq!(editor.tangents, original);
        assert_eq!(editor.keyframe_indices.len(), 1);
        assert!(matches!(
            events.borrow().last(),
            Some(CurveGestureEvent::Cancel(_))
        ));
    }

    #[test]
    fn non_live_virtual_boundary_emits_one_insert_with_the_reserved_identity() {
        let edits = std::rc::Rc::new(RefCell::new(Vec::new()));
        let recorded = edits.clone();
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.5), CurvePoint::new(1.0, 0.5)])
                .with_keyframe_ids(vec![None, None])
                .on_edit(move |edit| {
                    recorded.borrow_mut().push(edit);
                    None::<Action>
                });
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let start = editor.to_screen(editor.points[0]);
        let moved = Point::new(start.x, start.y - 12.0);
        let mut ctx = event_ctx();
        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let owner = editor.point_target(0).expect("reserved author target");
        editor.event(
            &UiEvent::MouseMove { position: moved, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(edits.borrow().is_empty());
        editor.event(
            &UiEvent::MouseUp {
                position: moved,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(
            matches!(edits.borrow().as_slice(), [CurveEdit::Insert { keyframe_id, index: 0, .. }]
            if owner == CurvePointTarget::Keyframe(*keyframe_id))
        );
        assert_eq!(editor.point_target(0), Some(owner));
        assert_eq!(
            editor.point_metadata[0].policy,
            CurvePointPolicy::editable()
        );
    }

    #[test]
    fn virtual_boundary_return_to_origin_cancels_reserved_author_identity() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.5), CurvePoint::new(1.0, 0.5)])
                .with_keyframe_ids(vec![None, None])
                .on_gesture(move |event| {
                    recorded.borrow_mut().push(event);
                    None::<Action>
                });
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let origin = editor.to_screen(editor.points[0]);
        let mut ctx = event_ctx();
        editor.event(
            &UiEvent::MouseDown {
                position: origin,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let id = match editor.point_target(0).expect("reserved target") {
            CurvePointTarget::Keyframe(id) => id,
            _ => panic!("reserved identity"),
        };
        for position in [Point::new(origin.x, origin.y - 12.0), origin] {
            editor.event(
                &UiEvent::MouseMove { position, modifiers: Modifiers::none() },
                &mut ctx,
            );
        }
        editor.event(
            &UiEvent::MouseUp {
                position: origin,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            editor.point_target(0),
            Some(CurvePointTarget::VirtualBoundary(CurveBoundary::Start))
        );
        assert!(editor.keyframe_indices.is_empty());
        let events = events.borrow();
        assert!(matches!(events.last(), Some(CurveGestureEvent::Cancel(_))));
        assert!(events
            .iter()
            .filter_map(|event| match event {
                CurveGestureEvent::Edit { edit: CurveEdit::Insert { keyframe_id, .. }, .. } =>
                    Some(*keyframe_id),
                _ => None,
            })
            .all(|key| key == id));
    }

    #[test]
    fn temporal_handle_reports_active_capture() {
        let mut editor = tangent_editor(std::rc::Rc::new(RefCell::new(Vec::new())));
        grab_tangent(&mut editor, &mut event_ctx());
        assert!(
            editor.has_active_pointer_interaction(),
            "router must retain this widget during a tangent draft"
        );
    }

    #[test]
    fn temporal_handle_disable_and_replacement_cancel_the_owned_draft() {
        for replacement in [false, true] {
            let events = std::rc::Rc::new(RefCell::new(Vec::new()));
            let mut editor = tangent_editor(events.clone());
            let original = editor.tangents.clone();
            let mut ctx = event_ctx();
            let start = grab_tangent(&mut editor, &mut ctx);
            let CurveGestureEvent::Begin(id) = events.borrow()[0] else {
                panic!("begin");
            };
            editor.event(
                &UiEvent::MouseMove {
                    position: Point::new(start.x + 20.0, start.y - 20.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            if replacement {
                editor.set_points(editor.points().to_vec());
            } else {
                editor.set_enabled(false);
            }
            editor.event(
                &UiEvent::MouseMove { position: start, modifiers: Modifiers::none() },
                &mut ctx,
            );
            assert_eq!(
                events.borrow().last(),
                Some(&CurveGestureEvent::Cancel(id)),
                "replacement={replacement}"
            );
            assert!(!editor.has_active_pointer_interaction());
            if !replacement {
                assert_eq!(editor.tangents, original);
            }
        }
    }

    #[test]
    fn temporal_handle_pointer_keeps_world_value_excursions_and_restores_on_escape() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut editor = tangent_editor(events.clone());
        let original = editor.tangents.clone();
        let original_points = editor.points().to_vec();
        let mut ctx = event_ctx();
        let start = grab_tangent(&mut editor, &mut ctx);
        let CurveGestureEvent::Begin(id) = events.borrow()[0] else {
            panic!("begin");
        };
        editor.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x + 500.0, start.y - 200.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(editor.tangents[0].point.x, 1.0);
        assert!(editor.tangents[0].point.y > 1.0);
        assert_eq!(editor.points(), original_points);
        editor.event(
            &UiEvent::MouseMove { position: start, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.tangents, original);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(events.borrow().last(), Some(&CurveGestureEvent::Cancel(id)));
        assert_eq!(editor.tangents, original);
        let count = events.borrow().len();
        editor.event(
            &UiEvent::MouseUp {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(events.borrow().len(), count);
    }

    #[test]
    fn set_points_clamps_sorts_and_anchors_endpoints() {
        let editor = CurveEditor::with_points(vec![
            CurvePoint { x: 1.4, y: -1.0 },
            CurvePoint { x: 0.5, y: 0.25 },
            CurvePoint { x: -0.2, y: 2.0 },
        ]);

        assert_eq!(editor.points()[0], CurvePoint::new(0.0, 1.0));
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.25));
        assert_eq!(editor.points()[2], CurvePoint::new(1.0, 0.0));
    }

    #[test]
    fn author_value_pointer_edits_beyond_soft_bounds_cancel_or_finish_in_one_identity() {
        use mondrian_core::automation::ParameterNumericRange;
        let axis = CurveValueAxis::new(
            ParameterNumericRange::new(0.0, 1.0).expect("soft"),
            ParameterNumericRange::new(-2.0, 2.0).expect("hard"),
            None,
        )
        .expect("axis");
        for cancel in [false, true] {
            let id = mondrian_core::KeyframeId::new();
            let original = CurvePoint { x: 0.5, y: 1.5000000000000002 };
            let events = std::rc::Rc::new(RefCell::new(Vec::new()));
            let recorded = events.clone();
            let mut editor = CurveEditor::with_author_points(
                vec![
                    CurvePoint { x: 0.0, y: original.y },
                    original,
                    CurvePoint { x: 1.0, y: original.y },
                ],
                axis,
            )
            .with_keyframe_ids(vec![None, Some(id), None])
            .on_gesture(move |event| {
                recorded.borrow_mut().push(event);
                None::<Action>
            });
            editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
            let mut focus = CurveFocus(Some(editor.id()));
            let mut shortcuts = DummyShortcut;
            let mut tooltip = DummyTooltip;
            let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &|_| {});
            editor.event(
                &UiEvent::FocusGained { source: FocusSource::Keyboard },
                &mut ctx,
            );
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
                &mut ctx,
            );
            editor.event(
                &UiEvent::KeyDown {
                    key: KeyCode::Right,
                    modifiers: Modifiers { ctrl: true, ..Modifiers::none() },
                },
                &mut ctx,
            );
            assert_eq!(editor.selected_keyframe(), Some(id));
            let grab = editor.to_screen(original);
            assert!(
                editor.plot_rect().contains(grab),
                "keyboard reveal exposes the legal out-of-soft key"
            );
            editor.event(
                &UiEvent::MouseDown {
                    position: grab,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            let moved = Point::new(grab.x, grab.y - 6.0);
            editor.event(
                &UiEvent::MouseMove { position: moved, modifiers: Modifiers::none() },
                &mut ctx,
            );
            assert!(editor.points[1].y > original.y && editor.points[1].y <= 2.0);
            assert_eq!(editor.points[1].x, original.x);
            if cancel {
                editor.event(
                    &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                    &mut ctx,
                );
                assert_eq!(editor.points[1].y.to_bits(), original.y.to_bits());
                assert!(matches!(
                    events.borrow().last(),
                    Some(CurveGestureEvent::Cancel(_))
                ));
            } else {
                editor.event(
                    &UiEvent::MouseUp {
                        position: moved,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                );
                assert!(matches!(
                    events.borrow().last(),
                    Some(CurveGestureEvent::Finish(_))
                ));
            }
            assert_eq!(editor.point_target(1), Some(CurvePointTarget::Keyframe(id)));
            assert!(!editor.has_active_pointer_interaction());
            assert_eq!(
                events
                    .borrow()
                    .iter()
                    .filter(|event| matches!(event, CurveGestureEvent::Begin(_)))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn temporal_curve_insertion_between_dense_keys_cancels_without_residue() {
        use mondrian_core::automation::ParameterNumericRange;
        let range = ParameterNumericRange::new(0.0, 1.0).expect("range");
        let axis = CurveValueAxis::new(range, range, None).expect("axis");
        let original = vec![
            CurvePoint::new(0.0, 0.5),
            CurvePoint::new(0.4995, 0.5),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(0.5005, 0.5),
            CurvePoint::new(1.0, 0.5),
        ];
        let mut editor = CurveEditor::with_author_points(original.clone(), axis);
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        editor.viewport.time_start = 0.499;
        editor.viewport.time_span = 0.002;
        let position = editor.to_screen(CurvePoint::new(0.49975, 0.8));
        let mut ctx = event_ctx();
        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(editor.points.len(), original.len() + 1);
        assert_eq!(editor.selected, Some(2));
        assert!((editor.points[2].x - 0.49975).abs() < 0.0000001);
        assert!(editor.has_active_pointer_interaction());
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.points, original);
        assert!(!editor.has_active_pointer_interaction());
    }

    #[test]
    fn temporal_snapshot_gesture_crossing_cancel_restores_origin_order() {
        use mondrian_core::automation::ParameterNumericRange;
        let range = ParameterNumericRange::new(0.0, 1.0).expect("range");
        let original = vec![
            CurvePoint::new(0.0, 0.5),
            CurvePoint::new(0.25, 0.5),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 0.5),
        ];
        let mut editor = CurveEditor::with_author_points(
            original.clone(),
            CurveValueAxis::new(range, range, None).expect("axis"),
        )
        .on_gesture(|_| None::<Action>);
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let start = editor.to_screen(original[1]);
        let destination = editor.to_screen(CurvePoint::new(0.75, 0.5));
        let mut ctx = event_ctx();
        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseMove {
                position: destination,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(editor.points[2], CurvePoint::new(0.75, 0.5));
        assert_eq!(editor.point_target(2), Some(CurvePointTarget::Snapshot(1)));
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.points, original);
        assert_eq!(editor.selected_index(), Some(1));
        assert!(!editor.has_active_pointer_interaction());
    }

    #[test]
    fn temporal_key_pointer_crossing_keeps_identity_capture_and_cancel_origin() {
        use mondrian_core::automation::ParameterNumericRange;
        let range = ParameterNumericRange::new(0.0, 1.0).expect("range");
        let original = vec![
            CurvePoint::new(0.0, 0.5),
            CurvePoint::new(0.25, 0.5),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 0.5),
        ];
        let ids = (0..4).map(|_| mondrian_core::KeyframeId::new()).collect::<Vec<_>>();
        let mut editor = CurveEditor::with_author_points(
            original.clone(),
            CurveValueAxis::new(range, range, None).expect("axis"),
        )
        .with_keyframe_ids(ids.iter().copied().map(Some).collect());
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let start = editor.to_screen(original[1]);
        let destination = editor.to_screen(CurvePoint::new(0.75, 0.5));
        let mut ctx = event_ctx();
        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseMove {
                position: destination,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(editor.points[2], CurvePoint::new(0.75, 0.5));
        assert_eq!(editor.selected_index(), Some(2));
        assert_eq!(
            editor.point_target(2),
            Some(CurvePointTarget::Keyframe(ids[1]))
        );
        assert_eq!(
            editor.point_target(1),
            Some(CurvePointTarget::Keyframe(ids[2]))
        );
        assert!(editor.has_active_pointer_interaction());
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.points, original);
        assert_eq!(editor.selected_index(), Some(1));
        for (index, id) in ids.into_iter().enumerate() {
            assert_eq!(
                editor.point_target(index),
                Some(CurvePointTarget::Keyframe(id))
            );
        }
        assert!(!editor.has_active_pointer_interaction());
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.selected_index(), None);
    }

    #[test]
    fn temporal_curve_drag_can_edit_dense_distinct_author_times() {
        use mondrian_core::automation::ParameterNumericRange;
        let range = ParameterNumericRange::new(0.0, 1.0).expect("range");
        let axis = CurveValueAxis::new(range, range, None).expect("axis");
        let original = vec![
            CurvePoint::new(0.0, 0.5),
            CurvePoint::new(0.4995, 0.5),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(0.5005, 0.5),
            CurvePoint::new(1.0, 0.5),
        ];
        let mut editor = CurveEditor::with_author_points(original.clone(), axis);
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        editor.viewport.time_start = 0.499;
        editor.viewport.time_span = 0.002;
        let start = editor.to_screen(editor.points[2]);
        let destination = editor.to_screen(CurvePoint::new(0.5001, 0.5));
        let mut ctx = event_ctx();
        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(editor.selected, Some(2));
        editor.event(
            &UiEvent::MouseMove {
                position: destination,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(
            (editor.points[2].x - 0.5001).abs() < 0.0000001,
            "zoomed temporal keys must not inherit transfer-curve display spacing: {:?}",
            editor.points[2]
        );
        assert_eq!(editor.points[1], original[1]);
        assert_eq!(editor.points[3], original[3]);
        assert!(editor.has_active_pointer_interaction());
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.points, original);
        assert!(!editor.has_active_pointer_interaction());
    }

    #[test]
    fn fixed_soft_bounds_keep_author_value_editing_and_reset_to_a_resolvable_view() {
        use mondrian_core::automation::ParameterNumericRange;
        let axis = CurveValueAxis::new(
            ParameterNumericRange::new(0.0, 0.0).expect("soft"),
            ParameterNumericRange::new(-2.0, 2.0).expect("hard"),
            Some(2.0),
        )
        .expect("axis");
        let mut editor = CurveEditor::with_author_points(
            vec![CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 1.0, y: 0.0 }],
            axis,
        );
        assert_eq!(editor.fixed_value, None);
        assert_eq!(editor.viewport.value_radius, 2.0);
        assert!(editor.move_point(0, CurvePoint { x: 0.0, y: 1.0 }));
        assert_eq!(editor.points[0].y, 1.0);
        assert_eq!(editor.value_step(false), 2.0);
    }

    #[test]
    fn author_points_retain_absolute_values_and_constrain_only_key_input() {
        use mondrian_core::automation::ParameterNumericRange;
        let axis = CurveValueAxis::new(
            ParameterNumericRange::new(0.0, 1.0).expect("soft"),
            ParameterNumericRange::new(-2.0, 2.0).expect("hard"),
            Some(0.1),
        )
        .expect("axis");
        let mut editor = CurveEditor::with_author_points(
            vec![
                CurvePoint { x: 0.0, y: 1.5 },
                CurvePoint { x: 1.0, y: -1.5 },
            ],
            axis,
        );
        assert_eq!(editor.points[0].y, 1.5);
        editor.set_display_points(vec![
            CurvePoint { x: 0.0, y: 3.0 },
            CurvePoint { x: 1.0, y: -3.0 },
        ]);
        assert_eq!(editor.display_points.as_ref().expect("samples")[0].y, 3.0);
        assert!(editor.move_point(0, CurvePoint { x: 0.0, y: 2.5 }));
        assert_eq!(editor.points[0].y, 2.0);
        assert_eq!(editor.value_step(false), 0.1);
        assert_eq!(editor.value_step(true), 0.5);
        editor.set_points(vec![
            CurvePoint { x: 0.0, y: 1.5000000000000002 },
            CurvePoint { x: 1.0, y: -0.0 },
        ]);
        assert_eq!(
            editor.points[0].y.to_bits(),
            1.5000000000000002f64.to_bits()
        );
        assert_eq!(editor.points[1].y.to_bits(), (-0.0f64).to_bits());
        let bounded = CurveEditor::with_points(vec![
            CurvePoint { x: 0.0, y: 1.5 },
            CurvePoint { x: 1.0, y: -1.5 },
        ]);
        assert_eq!(bounded.points[0].y, 1.0);
        assert_eq!(bounded.points[1].y, 0.0);
    }

    #[test]
    fn fixed_value_survives_programmatic_and_display_replacement() {
        let points = vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)];
        for fixed in [0.0, 0.5, 1.0] {
            let mut editor = CurveEditor::with_points(points.clone())
                .with_display_points(points.clone())
                .with_fixed_value(fixed);
            editor.set_points(vec![CurvePoint::new(0.4, 0.25)]);
            editor.set_display_points(points.clone());
            assert_eq!(
                editor.points(),
                &[CurvePoint::new(0.0, fixed), CurvePoint::new(1.0, fixed)]
            );
            assert!(editor
                .display_points
                .as_ref()
                .expect("display samples")
                .iter()
                .all(|point| point.y == fixed));
        }
        for invalid in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            let editor =
                CurveEditor::with_points(points.clone()).with_fixed_value(f64::from(invalid));
            assert_eq!(editor.points(), points);
            assert_eq!(editor.fixed_value, None);
        }
    }

    #[test]
    fn fixed_value_consumes_value_keys_but_retains_time_and_delete_edits() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let recorded = std::rc::Rc::clone(&events);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.5),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 0.5),
        ])
        .with_fixed_value(0.5)
        .on_gesture(move |event| {
            recorded.borrow_mut().push(event);
            None::<Action>
        });
        editor.select(Some(1));
        let mut ctx = event_ctx();
        for key in [KeyCode::Up, KeyCode::Down] {
            for modifiers in [Modifiers::none(), Modifiers::shift()] {
                assert_eq!(
                    editor.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                    EventResult::Handled
                );
            }
            for modifiers in [
                Modifiers::ctrl(),
                Modifiers { alt: true, ..Default::default() },
                Modifiers { meta: true, ..Default::default() },
            ] {
                assert_eq!(
                    editor.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                    EventResult::Ignored
                );
            }
        }
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
        assert!(events.borrow().is_empty());
        assert_eq!(
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
                &mut ctx
            ),
            EventResult::Handled
        );
        assert!(editor.points()[1].x > 0.5);
        assert_eq!(editor.points()[1].y, 0.5);
        assert!(matches!(
            events.borrow().as_slice(),
            [CurveGestureEvent::Edit { gesture_id: None, edit: CurveEdit::Move { .. } }]
        ));
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.points().len(), 2);
        assert!(matches!(
            events.borrow()[1],
            CurveGestureEvent::Edit {
                gesture_id: None,
                edit: CurveEdit::Delete { target: CurvePointTarget::Snapshot(1) }
            }
        ));
    }

    #[test]
    fn fixed_value_pointer_gestures_keep_time_edits_atomic_and_cancellable() {
        for mode in 0..5 {
            let events = std::rc::Rc::new(RefCell::new(Vec::new()));
            let recorded = std::rc::Rc::clone(&events);
            let original = vec![
                CurvePoint::new(0.0, 0.5),
                CurvePoint::new(0.5, 0.5),
                CurvePoint::new(1.0, 0.5),
            ];
            let mut editor = CurveEditor::with_points(original.clone())
                .with_fixed_value(0.5)
                .on_gesture(move |event| {
                    recorded.borrow_mut().push(event);
                    None::<Action>
                });
            editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
            let start = if mode >= 3 {
                Point::new(160.0, 27.0)
            } else {
                editor.to_screen(editor.points()[1])
            };
            let mut ctx = event_ctx();
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            let CurveGestureEvent::Begin(id) = events.borrow()[0] else {
                panic!("gesture begin");
            };
            editor.event(
                &UiEvent::MouseMove {
                    position: Point::new(start.x + if mode == 0 { 0.0 } else { 20.0 }, 12.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            assert!(editor.points().iter().all(|point| point.y == 0.5));
            assert_eq!(events.borrow().len(), [1, 2, 2, 3, 3][mode], "live updates");
            let cancel = mode == 2 || mode == 4;
            let terminal = if cancel {
                UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() }
            } else {
                UiEvent::MouseUp {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                }
            };
            editor.event(&terminal, &mut ctx);
            assert!(!editor.has_active_pointer_interaction());
            if cancel || mode == 0 {
                assert_eq!(editor.points(), original);
                assert_eq!(
                    events.borrow().last().cloned(),
                    Some(if cancel {
                        CurveGestureEvent::Cancel(id)
                    } else {
                        CurveGestureEvent::Finish(id)
                    })
                );
            } else {
                assert_eq!(events.borrow().len(), if mode == 3 { 5 } else { 4 });
                assert_eq!(
                    events.borrow().last().cloned(),
                    Some(CurveGestureEvent::Finish(id))
                );
                assert!(
                    matches!(events.borrow()[1], CurveGestureEvent::Edit { gesture_id: Some(edit_id), .. } if edit_id == id)
                );
                if mode == 3 {
                    assert_eq!(editor.points().len(), 4);
                } else {
                    assert!(editor.points()[1].x > 0.5);
                }
            }
            let count = events.borrow().len();
            assert_eq!(count, [2, 4, 3, 5, 4][mode]);
            if mode >= 3 {
                let recorded = events.borrow();
                let mut inserted_id = None;
                for event in &recorded[1..count - 1] {
                    let CurveGestureEvent::Edit {
                        gesture_id: Some(edit_id),
                        edit: CurveEdit::Insert { keyframe_id, point, .. },
                    } = event
                    else {
                        panic!("insert preview must retain one point identity");
                    };
                    assert_eq!(*edit_id, id);
                    assert_eq!(point.y, 0.5);
                    assert_eq!(*keyframe_id, *inserted_id.get_or_insert(*keyframe_id));
                }
            }
            editor.event(&UiEvent::FocusLost, &mut ctx);
            assert_eq!(
                events.borrow().len(),
                count,
                "terminal event is published once"
            );
        }
    }

    #[test]
    fn set_points_expands_single_point_into_anchored_curve() {
        let editor = CurveEditor::with_points(vec![CurvePoint::new(0.4, 0.25)]);

        assert_eq!(
            editor.points(),
            &[CurvePoint::new(0.0, 0.25), CurvePoint::new(1.0, 0.25)]
        );
    }

    #[test]
    fn right_click_opens_key_context_menu_without_editing_curve() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_point_context_menus(vec![
            None,
            Some(vec![MenuItem::new("自动贝塞尔", Action::Paste)]),
            None,
        ]);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let middle = editor.to_screen(editor.points()[1]);
        let mut ctx = event_ctx();
        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: middle,
                    button: MouseButton::Right,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(editor.context_menu.is_some());
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
    }

    #[test]
    fn clicking_empty_plot_inserts_and_selects_point() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)])
                .on_change(curve_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));

        let result = editor.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 50.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(editor.points().len(), 3);
        assert_eq!(editor.selected_index(), Some(1));
        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_action(editor.points())]
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn clicking_outside_plot_padding_does_not_insert_point() {
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let mut ctx = event_ctx();

        editor.event(
            &UiEvent::MouseDown {
                position: Point::new(4.0, 4.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(editor.points().len(), 2);
    }

    #[test]
    fn insertion_limit_preserves_editing_and_deletion_frees_capacity() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let original = vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ];
        let mut editor = CurveEditor::with_points(original.clone())
            .with_point_insertion_limit(3)
            .on_change(curve_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        editor.select(Some(1));
        let empty = editor.to_screen(CurvePoint::new(0.25, 0.75));
        editor.event(
            &UiEvent::MouseDown {
                position: empty,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(editor.points(), original);
        assert_eq!(editor.selected_index(), Some(1));
        assert!(!editor.has_active_pointer_interaction());
        assert!(ctx.requests.pointer_capture.is_none());
        assert!(actions.borrow().is_empty());

        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(editor.points()[1].y > 0.5);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.points().len(), 2);
        editor.event(
            &UiEvent::MouseDown {
                position: empty,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(editor.points().len(), 3);
        assert!(editor.has_active_pointer_interaction());
        assert!(ctx.requests.pointer_capture.is_some());
        assert!(!actions.borrow().is_empty());
    }

    #[test]
    fn insertion_limit_retains_supplied_points_and_default_is_unlimited() {
        let points: Vec<_> = (0..32)
            .map(|index| CurvePoint::new(index as f32 / 31.0, index as f64 / 31.0))
            .collect();
        let limited = CurveEditor::with_points(points.clone()).with_point_insertion_limit(0);
        assert_eq!(limited.points(), points);
        let mut editor = CurveEditor::with_points(points);
        editor.layout(Rect::new(0.0, 0.0, 400.0, 200.0));
        let empty = editor.to_screen(CurvePoint::new(0.49, 0.75));
        editor.event(
            &UiEvent::MouseDown {
                position: empty,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut event_ctx(),
        );
        assert_eq!(editor.points().len(), 33);
        assert!(editor.has_active_pointer_interaction());
    }

    #[test]
    fn dragging_point_updates_value_and_releases_capture() {
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);
        let target = Point::new(190.0, 90.0);
        let mut ctx = event_ctx();

        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Capture(
                editor.id()
            ))
        );
        assert!(editor.has_active_pointer_interaction());
        editor.event(
            &UiEvent::MouseMove { position: target, modifiers: Modifiers::none() },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseUp {
                position: target,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
        assert!(!editor.has_active_pointer_interaction());
        assert_eq!(editor.points()[1].x, 1.0);
        assert!(editor.points()[1].y < 0.2);
    }

    #[test]
    fn single_axis_drag_preserves_grab_offset_and_untouched_coordinate() {
        for horizontal in [true, false] {
            let original = CurvePoint::new(0.33333334, 0.73123455);
            let mut editor = CurveEditor::with_points(vec![
                CurvePoint::new(0.0, 0.0),
                original,
                CurvePoint::new(1.0, 1.0),
            ]);
            editor.layout(Rect::new(17.0, 23.0, 317.0, 173.0));
            let center = editor.to_screen(original);
            let grab = Point::new(center.x + 2.0, center.y + 2.0);
            let mut ctx = event_ctx();
            editor.event(
                &UiEvent::MouseDown {
                    position: grab,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            assert_eq!(editor.selected_index(), Some(1));
            editor.event(
                &UiEvent::MouseMove { position: grab, modifiers: Modifiers::none() },
                &mut ctx,
            );
            assert_eq!(
                editor.points()[1],
                original,
                "stationary pointer must not move the grabbed point"
            );
            let target = if horizontal {
                Point::new(grab.x + 20.0, grab.y)
            } else {
                Point::new(grab.x, grab.y - 20.0)
            };
            editor.event(
                &UiEvent::MouseMove { position: target, modifiers: Modifiers::none() },
                &mut ctx,
            );
            if horizontal {
                assert_eq!(editor.points()[1].y, original.y);
                assert!(editor.points()[1].x > original.x);
            } else {
                assert_eq!(editor.points()[1].x, original.x);
                assert!(editor.points()[1].y > original.y);
            }
        }
    }

    #[test]
    fn resizing_a_captured_curve_to_collapsed_axes_keeps_coordinates_finite() {
        for (width, height) in [(10.0, 173.0), (317.0, 10.0), (10.0, 10.0)] {
            let original = CurvePoint::new(0.33333334, 0.73123455);
            let mut editor = CurveEditor::with_points(vec![
                CurvePoint::new(0.0, 0.0),
                original,
                CurvePoint::new(1.0, 1.0),
            ]);
            editor.layout(Rect::new(17.0, 23.0, 317.0, 173.0));
            let grab = editor.to_screen(original);
            let mut ctx = event_ctx();
            editor.event(
                &UiEvent::MouseDown {
                    position: grab,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            editor.layout(Rect::new(17.0, 23.0, width, height));
            editor.event(
                &UiEvent::MouseMove { position: grab, modifiers: Modifiers::none() },
                &mut ctx,
            );
            assert_eq!(
                editor.points()[1],
                original,
                "collapsed geometry must preserve a stationary point"
            );
            let target = Point::new(grab.x + 20.0, grab.y - 20.0);
            editor.event(
                &UiEvent::MouseMove { position: target, modifiers: Modifiers::none() },
                &mut ctx,
            );
            assert!(editor.points()[1].x.is_finite() && editor.points()[1].y.is_finite());
            if width == 10.0 {
                assert_eq!(editor.points()[1].x, original.x);
            }
            if height == 10.0 {
                assert_eq!(editor.points()[1].y, original.y);
            }
            editor.event(
                &UiEvent::MouseUp {
                    position: target,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            assert!(!editor.has_active_pointer_interaction());
            assert_eq!(
                ctx.requests.pointer_capture,
                Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                    editor.id()
                ))
            );
        }
    }

    #[test]
    fn dragging_point_dispatches_changed_curve_and_requests_repaint() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);

        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x, start.y - 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_action(editor.points())]
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn accepted_author_delete_reveals_the_new_selection_after_removal() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.5),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 0.5),
        ])
        .with_preview_projection(|event| {
            matches!(
                event,
                CurveGestureEvent::Edit { edit: CurveEdit::Delete { .. }, .. }
            )
            .then(|| CurvePreview {
                virtual_boundaries: [None, None],
                display_points: vec![CurvePoint::new(0.0, 0.5), CurvePoint::new(1.0, 0.5)],
                key_points: Vec::new(),
                tangents: Vec::new(),
            })
        })
        .on_gesture(|_| None::<Action>);
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        editor.viewport.time_start = 0.45;
        editor.viewport.time_span = 0.1;
        editor.select(Some(1));
        let mut ctx = event_ctx();
        assert_eq!(
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
                &mut ctx
            ),
            EventResult::Handled
        );
        assert_eq!(editor.points.len(), 2);
        assert_eq!(editor.selected, Some(1));
        assert!(
            editor.plot_rect().contains(editor.to_screen(editor.points[1])),
            "the replacement selection must remain visible after canonical deletion"
        );
    }

    #[test]
    fn rejected_author_curve_edits_preserve_keyboard_handle_delete_and_insert_state() {
        for operation in 0..6 {
            let events = std::rc::Rc::new(RefCell::new(Vec::new()));
            let mut editor = tangent_editor(events.clone()).with_preview_projection(|_| None);
            if operation == 5 {
                editor.points[1].y = 1.0;
            }
            let original = editor.points.clone();
            let controls = editor.tangents.clone();
            let identities = editor.keyframe_indices.clone();
            let selection = editor.selected;
            let mut ctx = event_ctx();
            match operation {
                0 => {
                    editor.event(
                        &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
                        &mut ctx,
                    );
                }
                1 => {
                    editor.event(
                        &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
                        &mut ctx,
                    );
                }
                2 => {
                    editor.selected_tangent = Some((controls[0].keyframe_id, controls[0].side));
                    assert_eq!(
                        editor.event(
                            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
                            &mut ctx,
                        ),
                        EventResult::Handled,
                        "a rejected handle edit must not leak its arrow key"
                    );
                }
                3 => {
                    let start = editor.to_screen(controls[0].point);
                    editor.event(
                        &UiEvent::MouseDown {
                            position: start,
                            button: MouseButton::Left,
                            modifiers: Modifiers::none(),
                        },
                        &mut ctx,
                    );
                    editor.event(
                        &UiEvent::MouseMove {
                            position: Point::new(start.x, start.y - 10.0),
                            modifiers: Modifiers::none(),
                        },
                        &mut ctx,
                    );
                    assert!(editor.has_active_pointer_interaction());
                }
                4 => {
                    let position = editor.to_screen(CurvePoint::new(0.25, 0.75));
                    editor.event(
                        &UiEvent::MouseDown {
                            position,
                            button: MouseButton::Left,
                            modifiers: Modifiers::none(),
                        },
                        &mut ctx,
                    );
                    assert!(!editor.has_active_pointer_interaction());
                }
                _ => {
                    assert_eq!(
                        editor.event(
                            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
                            &mut ctx,
                        ),
                        EventResult::Handled,
                        "a clamped point nudge retains keyboard ownership"
                    );
                }
            }
            assert_eq!(
                editor.points, original,
                "operation {operation} retains admitted keys"
            );
            assert_eq!(
                editor.tangents, controls,
                "operation {operation} retains admitted controls"
            );
            assert_eq!(editor.keyframe_indices, identities);
            assert_eq!(editor.selected, selection);
            assert!(
                !events
                    .borrow()
                    .iter()
                    .any(|event| matches!(event, CurveGestureEvent::Edit { .. })),
                "rejected edits cannot dispatch author candidates"
            );
        }
    }

    #[test]
    fn rejected_author_preview_restores_last_admitted_geometry_and_keeps_capture() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let captured = events.clone();
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_preview_projection(|event| match event {
            CurveGestureEvent::Edit { edit: CurveEdit::Move { point, .. }, .. }
                if point.y <= 0.6 =>
            {
                Some(CurvePreview {
                    virtual_boundaries: [None, None],
                    key_points: Vec::new(),
                    display_points: vec![
                        CurvePoint::new(0.0, 0.0),
                        point,
                        CurvePoint::new(1.0, 1.0),
                    ],
                    tangents: Vec::new(),
                })
            }
            _ => None,
        })
        .on_gesture(move |event| {
            captured.borrow_mut().push(event);
            None
        });
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let mut ctx = event_ctx();
        let start = editor.to_screen(editor.points()[1]);
        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let valid = editor.to_screen(CurvePoint::new(0.55, 0.55));
        editor.event(
            &UiEvent::MouseMove { position: valid, modifiers: Modifiers::none() },
            &mut ctx,
        );
        let admitted = editor.points.clone();
        let samples = editor.display_points.clone();
        let invalid = editor.to_screen(CurvePoint::new(0.6, 0.8));
        editor.event(
            &UiEvent::MouseMove { position: invalid, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            editor.points, admitted,
            "rejected author candidate cannot leave a ghost key"
        );
        assert_eq!(editor.display_points, samples);
        assert!(
            editor.has_active_pointer_interaction(),
            "rejection retains the pointer gesture"
        );
        editor.event(
            &UiEvent::MouseMove { position: valid, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            editor.points, admitted,
            "the same gesture can recover to a valid candidate"
        );
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
        assert!(matches!(
            events.borrow().last(),
            Some(CurveGestureEvent::Cancel(_))
        ));
    }

    #[test]
    fn author_gesture_emits_current_key_position_before_pointer_release() {
        let events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let captured = events.clone();
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let projected_events = std::rc::Rc::new(RefCell::new(Vec::new()));
        let projected = projected_events.clone();
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_display_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .with_preview_projection(move |event| {
            projected.borrow_mut().push(event.clone());
            match event {
                CurveGestureEvent::Edit { edit: CurveEdit::Move { point, .. }, .. } => {
                    Some(CurvePreview {
                        virtual_boundaries: [None, None],
                        key_points: Vec::new(),
                        display_points: vec![
                            CurvePoint::new(0.0, 0.0),
                            point,
                            CurvePoint::new(1.0, 1.0),
                        ],
                        tangents: Vec::new(),
                    })
                }
                CurveGestureEvent::Cancel(_) => Some(CurvePreview {
                    virtual_boundaries: [None, None],
                    key_points: Vec::new(),
                    display_points: vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 0.0)],
                    tangents: Vec::new(),
                }),
                _ => None,
            }
        })
        .on_gesture(move |event| {
            captured.borrow_mut().push(event);
            None
        });
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let initial_display = editor.display_points.clone();
        let start = editor.to_screen(editor.points()[1]);
        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let CurveGestureEvent::Begin(id) = events.borrow()[0] else {
            panic!("pointer drag must open the author gesture");
        };
        for position in [
            Point::new(start.x + 8.0, start.y - 5.0),
            Point::new(start.x + 18.0, start.y - 12.0),
        ] {
            editor.event(
                &UiEvent::MouseMove { position, modifiers: Modifiers::none() },
                &mut ctx,
            );
            assert_eq!(
                events.borrow().last().cloned(),
                Some(CurveGestureEvent::Edit {
                    gesture_id: Some(id),
                    edit: CurveEdit::Move {
                        target: CurvePointTarget::Snapshot(1),
                        point: editor.points()[1]
                    },
                }),
                "current author coordinates must reach preview while the pointer is held",
            );
            assert_eq!(
                editor.display_points.as_ref().unwrap()[1],
                editor.points()[1]
            );
            assert!(ctx.requests.repaint);
        }
        assert_ne!(editor.display_points, initial_display);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            editor.display_points, initial_display,
            "cancel must restore the exact sampled line"
        );
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
        assert_eq!(
            events.borrow().last().cloned(),
            Some(CurveGestureEvent::Cancel(id))
        );
        assert_eq!(
            *projected_events.borrow(),
            *events.borrow(),
            "projection must receive the same Begin, Edit and Cancel lifecycle as authoring"
        );
        assert!(!editor.has_active_pointer_interaction());
    }

    #[test]
    fn committed_drag_emits_one_incremental_edit_only_on_release() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_edit(curve_edit_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);

        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        for position in [
            Point::new(start.x + 8.0, start.y - 5.0),
            Point::new(start.x + 18.0, start.y - 12.0),
        ] {
            editor.event(
                &UiEvent::MouseMove { position, modifiers: Modifiers::none() },
                &mut ctx,
            );
        }
        assert!(actions.borrow().is_empty());
        let committed = editor.points()[1];

        editor.event(
            &UiEvent::MouseUp {
                position: editor.to_screen(committed),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_edit_action(CurveEdit::Move {
                target: CurvePointTarget::Snapshot(1),
                point: committed,
            })]
        );
    }

    #[test]
    fn inserted_point_emits_one_incremental_edit_on_release() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)])
                .on_edit(curve_edit_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let position = Point::new(100.0, 50.0);

        editor.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(actions.borrow().is_empty());
        let inserted = editor.points()[1];
        editor.event(
            &UiEvent::MouseUp {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_edit_action(CurveEdit::Insert {
                keyframe_id: mondrian_core::KeyframeId::new(),
                index: 1,
                point: inserted,
            })]
        );
    }

    #[test]
    fn live_curve_value_edits_keep_insertion_updates_and_termination_in_one_identity() {
        for inserted in [false, true] {
            for terminal in 0..5 {
                let original = if inserted {
                    vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]
                } else {
                    vec![
                        CurvePoint::new(0.0, 0.0),
                        CurvePoint::new(0.5, 0.23),
                        CurvePoint::new(1.0, 1.0),
                    ]
                };
                let events = std::rc::Rc::new(RefCell::new(Vec::new()));
                let recorded = std::rc::Rc::clone(&events);
                let mut editor = CurveEditor::with_points(original.clone()).on_value_edit(
                    move |phase, points| {
                        recorded.borrow_mut().push((phase, points.to_vec()));
                        None::<Action>
                    },
                );
                editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
                let mut focus = DummyFocus;
                let mut shortcut = DummyShortcut;
                let mut tooltip = DummyTooltip;
                let dispatch = |_: Action| {};
                let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
                let start = editor.to_screen(if inserted {
                    CurvePoint::new(0.5, 0.5)
                } else {
                    original[1]
                });
                editor.event(
                    &UiEvent::MouseDown {
                        position: start,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                );
                let ValueEdit::Begin(id) = events.borrow()[0].0 else {
                    panic!("live pointer edit starts before the first mutation");
                };
                assert_eq!(events.borrow()[0].1, original);
                let moved = editor.to_screen(CurvePoint::new(0.6, 0.7));
                editor.event(
                    &UiEvent::MouseMove { position: moved, modifiers: Modifiers::none() },
                    &mut ctx,
                );
                assert_ne!(editor.points(), original);
                assert_eq!(
                    events.borrow().last().expect("live update").1,
                    editor.points()
                );
                assert!(events.borrow()[1..]
                    .iter()
                    .all(|(phase, _)| *phase == ValueEdit::Update(id)));
                if terminal == 3 {
                    editor.set_enabled(false);
                }
                let event = match terminal {
                    1 => UiEvent::FocusLost,
                    2 => UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                    3 => UiEvent::MouseMove { position: moved, modifiers: Modifiers::none() },
                    4 => UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
                    _ => UiEvent::MouseUp {
                        position: moved,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                };
                editor.event(&event, &mut ctx);
                let cancelled = terminal == 2 || terminal == 3 || (terminal == 4 && inserted);
                assert_eq!(
                    events.borrow().last().expect("terminal").0,
                    if cancelled {
                        ValueEdit::Cancel(id)
                    } else {
                        ValueEdit::Finish(id)
                    }
                );
                if cancelled {
                    assert_eq!(
                        editor.points(),
                        original,
                        "cancel restores exact pre-insertion/pre-move points"
                    );
                }
                let count = events.borrow().len();
                editor.event(&UiEvent::FocusLost, &mut ctx);
                editor.event(
                    &UiEvent::MouseUp {
                        position: moved,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                );
                assert_eq!(
                    events.borrow().len(),
                    count,
                    "late terminal input cannot publish an independent edit"
                );
            }
        }
    }

    #[test]
    fn curve_gesture_lifecycle_updates_author_and_finishes_or_cancels_once() {
        for finish in 0..6 {
            let events = std::rc::Rc::new(RefCell::new(Vec::new()));
            let recorded = std::rc::Rc::clone(&events);
            let mut editor = CurveEditor::with_points(vec![
                CurvePoint::new(0.0, 0.0),
                CurvePoint::new(0.5, 0.5),
                CurvePoint::new(1.0, 1.0),
            ])
            .on_gesture(move |event| {
                recorded.borrow_mut().push(event);
                None::<Action>
            });
            editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
            let start = editor.to_screen(editor.points()[1]);
            let mut ctx = event_ctx();
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            let CurveGestureEvent::Begin(id) = events.borrow()[0] else {
                panic!("begin");
            };
            if finish != 0 {
                editor.event(
                    &UiEvent::MouseMove {
                        position: Point::new(start.x + 20.0, start.y - 15.0),
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                );
                editor.event(
                    &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
                    &mut ctx,
                );
            }
            assert_eq!(
                events.borrow().len(),
                if finish == 0 { 1 } else { 3 },
                "pointer and keyboard changes publish current coordinates"
            );
            let terminal = match finish {
                2 => UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                3 => UiEvent::FocusLost,
                4 => {
                    editor.set_enabled(false);
                    UiEvent::MouseMove { position: start, modifiers: Modifiers::none() }
                }
                5 => UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
                _ => UiEvent::MouseUp {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
            };
            editor.event(&terminal, &mut ctx);
            assert!(!editor.has_active_pointer_interaction());
            assert_eq!(
                ctx.requests.pointer_capture,
                Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                    editor.id()
                ))
            );
            for event in [
                UiEvent::MouseUp {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                UiEvent::FocusLost,
            ] {
                editor.event(&event, &mut ctx);
            }
            let events = events.borrow();
            if finish == 2 || finish == 4 {
                assert_eq!(events.last().cloned(), Some(CurveGestureEvent::Cancel(id)));
                assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
            } else if finish == 0 {
                assert_eq!(
                    events.as_slice(),
                    &[CurveGestureEvent::Begin(id), CurveGestureEvent::Finish(id)]
                );
            } else {
                assert_eq!(events.len(), 5);
                assert!(
                    matches!(events[1], CurveGestureEvent::Edit { gesture_id: Some(edit_id), .. } if edit_id == id)
                );
                assert_eq!(events[4], CurveGestureEvent::Finish(id));
                if finish == 5 {
                    assert_eq!(editor.points().len(), 2);
                }
            }
        }
    }

    #[test]
    fn escape_releases_capture_for_unchanged_and_round_trip_curve_drags() {
        for round_trip in [false, true] {
            let actions = RefCell::new(Vec::new());
            let dispatch = |action: Action| actions.borrow_mut().push(action);
            let mut focus = DummyFocus;
            let mut shortcut = DummyShortcut;
            let mut tooltip = DummyTooltip;
            let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
            let original = CurvePoint::new(0.5, 0.5);
            let mut editor = CurveEditor::with_points(vec![
                CurvePoint::new(0.0, 0.0),
                original,
                CurvePoint::new(1.0, 1.0),
            ])
            .on_edit(curve_edit_action);
            editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
            let start = editor.to_screen(original);
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            assert!(editor.has_active_pointer_interaction());
            if round_trip {
                for position in [Point::new(start.x + 20.0, start.y - 15.0), start] {
                    editor.event(
                        &UiEvent::MouseMove { position, modifiers: Modifiers::none() },
                        &mut ctx,
                    );
                }
            }
            assert_eq!(editor.points()[1], original);
            *ctx.requests = Default::default();
            assert_eq!(
                editor.event(
                    &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                    &mut ctx
                ),
                EventResult::Handled
            );
            assert!(!editor.has_active_pointer_interaction());
            assert_eq!(
                ctx.requests.pointer_capture,
                Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                    editor.id()
                ))
            );
            assert!(ctx.requests.repaint);
            for event in [
                UiEvent::MouseMove {
                    position: Point::new(start.x + 30.0, start.y),
                    modifiers: Modifiers::none(),
                },
                UiEvent::MouseUp {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                UiEvent::FocusLost,
            ] {
                editor.event(&event, &mut ctx);
            }
            assert_eq!(editor.points()[1], original);
            assert!(actions.borrow().is_empty());
        }
    }

    #[test]
    fn escape_reverts_drag_without_committing_incremental_edit() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let original = CurvePoint::new(0.5, 0.5);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            original,
            CurvePoint::new(1.0, 1.0),
        ])
        .on_edit(curve_edit_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(original);

        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x + 20.0, start.y - 15.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_ne!(editor.points()[1], original);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(editor.points()[1], original);
        assert!(actions.borrow().is_empty());
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
    }

    #[test]
    fn dragging_point_to_same_position_does_not_dispatch_duplicate_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);

        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseMove { position: start, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn interior_point_cannot_cross_neighbors() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);

        assert!(editor.move_point(1, CurvePoint::new(1.2, 0.75)));
        assert!(editor.points()[1].x < editor.points()[2].x);
        assert!(editor.points()[1].x > editor.points()[0].x);
    }

    #[test]
    fn keyboard_nudges_selected_point() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.select(Some(1));
        let mut ctx = event_ctx();

        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
            &mut ctx,
        );

        assert!(editor.points()[1].y > 0.54);
    }

    #[test]
    fn keyboard_nudge_dispatches_changed_curve() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.select(Some(1));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_action(editor.points())]
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn keyboard_nudge_ignores_ctrl_alt_and_meta_chords() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.select(Some(1));

        for (key, modifiers) in [
            (KeyCode::Right, Modifiers::ctrl()),
            (KeyCode::Left, Modifiers { alt: true, ..Default::default() }),
            (KeyCode::Up, Modifiers { meta: true, ..Default::default() }),
            (
                KeyCode::Down,
                Modifiers { ctrl: true, shift: true, ..Default::default() },
            ),
        ] {
            assert_eq!(
                editor.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
        }

        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn disabled_editor_ignores_pointer_keyboard_and_focus() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action)
        .disabled();
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);

        assert!(!editor.is_enabled());
        assert!(!editor.can_focus());
        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert_eq!(
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
        assert!(actions.borrow().is_empty());
        assert!(ctx.requests.pointer_capture.is_none());
    }

    #[test]
    fn disabled_focused_editor_does_not_release_unowned_capture() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.focused = true;
        editor.set_enabled(false);
        let mut ctx = event_ctx();

        assert_eq!(
            editor.event(
                &UiEvent::MouseMove {
                    position: Point::new(100.0, 50.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(ctx.requests.pointer_capture, None);
        assert!(!editor.focused);
        assert!(editor.dragging.is_none());
    }

    #[test]
    fn disabling_while_dragging_releases_capture_on_next_event() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);
        let mut ctx = event_ctx();

        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        editor.set_enabled(false);
        assert!(editor.dragging.is_none());

        assert_eq!(
            editor.event(
                &UiEvent::MouseMove {
                    position: Point::new(start.x + 10.0, start.y),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
    }

    #[test]
    fn disabling_and_reenabling_cancels_pending_curve_drag() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);

        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(editor.dragging.map(|drag| drag.index), Some(1));
        ctx.requests.pointer_capture = None;

        editor.set_enabled(false);
        assert!(editor.dragging.is_none());
        editor.set_enabled(true);

        assert_eq!(
            editor.event(
                &UiEvent::MouseMove {
                    position: Point::new(start.x + 30.0, start.y - 20.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.5));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn focus_lost_without_drag_does_not_release_pointer_capture() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.select(Some(1));
        let mut ctx = event_ctx();

        assert_eq!(
            editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
            EventResult::Handled
        );
        assert_eq!(
            editor.event(&UiEvent::FocusLost, &mut ctx),
            EventResult::Handled
        );

        assert_eq!(ctx.requests.pointer_capture, None);
        assert!(!editor.focused);
        assert_eq!(editor.selected_index(), None);
    }

    #[test]
    fn focus_lost_during_drag_releases_pointer_capture() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);
        let mut ctx = event_ctx();

        editor.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        ctx.requests.pointer_capture = None;

        assert_eq!(
            editor.event(&UiEvent::FocusLost, &mut ctx),
            EventResult::Handled
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
        assert!(editor.dragging.is_none());
    }

    #[test]
    fn keyboard_nudge_at_endpoint_boundary_does_not_dispatch() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)])
                .on_change(curve_action);
        editor.select(Some(0));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Left, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            editor.points(),
            &[CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]
        );
        assert!(actions.borrow().is_empty());
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn delete_removes_selected_interior_point_and_dispatches_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ])
        .on_change(curve_action);
        editor.select(Some(1));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            editor.points(),
            &[CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_action(editor.points())]
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn delete_does_not_remove_endpoints() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)])
                .on_change(curve_action);
        editor.select(Some(0));

        let result = editor.event(
            &UiEvent::KeyDown {
                key: KeyCode::Backspace,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(editor.points().len(), 2);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn explicit_editable_boundary_point_can_move_and_emits_incremental_edit() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.25), CurvePoint::new(1.0, 0.75)])
                .with_point_policies(vec![
                    CurvePointPolicy::editable(),
                    CurvePointPolicy::anchor(),
                ])
                .on_edit(curve_edit_action);
        editor.select(Some(0));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(editor.points()[0], CurvePoint::new(0.01, 0.25));
        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_edit_action(CurveEdit::Move {
                target: CurvePointTarget::Snapshot(0),
                point: CurvePoint::new(0.01, 0.25),
            })]
        );
    }

    #[test]
    fn explicit_editable_boundary_point_can_be_deleted() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.25), CurvePoint::new(1.0, 0.75)])
                .with_point_policies(vec![
                    CurvePointPolicy::editable(),
                    CurvePointPolicy::anchor(),
                ])
                .on_edit(curve_edit_action);
        editor.select(Some(0));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(editor.points(), &[CurvePoint::new(1.0, 0.75)]);
        assert_eq!(editor.selected_index(), Some(0));
        assert_eq!(
            actions.borrow().as_slice(),
            &[curve_edit_action(CurveEdit::Delete {
                target: CurvePointTarget::Snapshot(0)
            })]
        );
    }

    #[test]
    fn mismatched_point_policies_preserve_safe_endpoint_defaults() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.25), CurvePoint::new(1.0, 0.75)])
                .with_point_policies(vec![CurvePointPolicy::editable()])
                .on_edit(curve_edit_action);
        editor.select(Some(0));

        let result = editor.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(editor.points().len(), 2);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn temporal_rulers_reserve_plot_space_and_paint_readable_owner_units() {
        let range = mondrian_core::TimelineTimeRange::new(
            mondrian_core::TimelineTime::new(10, 1).expect("origin"),
            mondrian_core::TimelineTime::new(2, 1).expect("duration"),
        )
        .expect("range");
        let mut editor = CurveEditor::with_author_points(
            vec![CurvePoint::new(0.0, 20.0), CurvePoint::new(1.0, 80.0)],
            CurveValueAxis::new(
                mondrian_core::automation::ParameterNumericRange::new(20.0, 80.0).expect("soft"),
                mondrian_core::automation::ParameterNumericRange::new(0.0, 100.0).expect("hard"),
                Some(1.0),
            )
            .expect("axis"),
        )
        .with_time_axis(range);
        let theme = ThemePreset::Dark.build();
        for bounds in [
            Rect::new(0.0, 0.0, 220.0, 104.0),
            Rect::new(7.0, 9.0, 80.0, 45.0),
        ] {
            editor.layout(bounds);
            let plot = editor.plot_bounds();
            assert!(plot.x > bounds.x + PADDING);
            assert!(plot.y + plot.height < bounds.y + bounds.height - PADDING);
            let mut encoder = RecordingEncoder::default();
            editor.paint(&mut PaintContext {
                encoder: &mut encoder,
                theme: &theme,
                clip_rect: bounds,
            });
            for (text, font, point) in &encoder.texts {
                let (width, _) = crate::text_metrics::measure_single_line(text, *font);
                assert!(point.x >= bounds.x && point.x + width <= bounds.x + bounds.width);
                assert!(!plot.contains(*point), "labels cannot cover editable keys");
            }
            if bounds.width > 100.0 {
                assert!(encoder.texts.iter().any(|(text, _, _)| text == "10s"));
                assert!(encoder.texts.iter().any(|(text, _, _)| text == "80"));
            }
        }
    }

    #[test]
    fn fixed_value_and_fractional_origin_rulers_do_not_invent_author_coordinates() {
        use mondrian_core::automation::ParameterNumericRange;
        use mondrian_core::{TimelineTime, TimelineTimeRange};
        let axis = CurveValueAxis::new(
            ParameterNumericRange::new(42.5, 42.5).expect("soft"),
            ParameterNumericRange::new(42.5, 42.5).expect("hard"),
            None,
        )
        .expect("axis");
        let mut editor = CurveEditor::with_author_points(
            vec![CurvePoint::new(0.0, 42.5), CurvePoint::new(1.0, 42.5)],
            axis,
        )
        .with_time_axis(
            TimelineTimeRange::new(TimelineTime::new(1, 30).expect("origin"), TimelineTime::ONE)
                .expect("range"),
        );
        editor.layout(Rect::new(0.0, 0.0, 300.0, 120.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        editor.paint(&mut PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: editor.bounds,
        });
        let values = encoder
            .texts
            .iter()
            .filter(|(text, _, _)| !text.ends_with('s'))
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].0, "42.5");
        assert!(encoder.texts.iter().any(|(text, _, _)| text == "0.033s"));
        assert!(editor.points.iter().all(|point| point.y == 42.5));
    }

    #[test]
    fn paint_draws_grid_curve_and_points() {
        let mut editor = CurveEditor::new();
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 220.0, 104.0),
        };

        editor.paint(&mut ctx);

        assert!(encoder.rects.len() >= 2 + editor.points().len() * 2);
        assert!(encoder.lines >= editor.grid_columns + editor.grid_rows + editor.points().len());
    }

    #[test]
    fn group_context_menu_preserves_selection_keyboard_pointer_and_escape() {
        for keyboard in [true, false] {
            let ids = [
                mondrian_core::KeyframeId::new(),
                mondrian_core::KeyframeId::new(),
                mondrian_core::KeyframeId::new(),
            ];
            let requests = std::rc::Rc::new(RefCell::new(Vec::new()));
            let menu_requests = requests.clone();
            let actions = RefCell::new(Vec::new());
            let dispatch = |action| actions.borrow_mut().push(action);
            let mut editor = CurveEditor::with_points(vec![
                CurvePoint::new(0.0, 0.2),
                CurvePoint::new(0.5, 0.7),
                CurvePoint::new(1.0, 0.3),
            ])
            .with_keyframe_ids(ids.map(Some).to_vec())
            .with_selection_context_menu(move |keys| {
                menu_requests.borrow_mut().push(keys.to_vec());
                vec![MenuItem::new("Auto", Action::Paste)]
            });
            editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
            let before = editor.points().to_vec();
            let mut focus = CurveFocus(Some(editor.id()));
            let mut shortcuts = DummyShortcut;
            let mut tooltip = DummyTooltip;
            let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
            editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
                &mut ctx,
            );
            assert_eq!(editor.selected_keyframe_ids(), ids);
            assert!(
                requests.borrow().is_empty(),
                "construction and selection do not prepare menu commands"
            );
            let open = if keyboard {
                UiEvent::KeyDown { key: KeyCode::F10, modifiers: Modifiers::shift() }
            } else {
                UiEvent::MouseDown {
                    position: editor.to_screen(editor.points()[1]),
                    button: MouseButton::Right,
                    modifiers: Modifiers::none(),
                }
            };
            assert_eq!(editor.event(&open, &mut ctx), EventResult::Handled);
            assert_eq!(*requests.borrow(), vec![ids.to_vec()]);
            assert_eq!(editor.selected_keyframe_ids(), ids);
            assert!(editor.context_menu.is_some());
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                &mut ctx,
            );
            assert!(editor.context_menu.is_none());
            assert!(actions.borrow().is_empty());
            assert_eq!(editor.selected_keyframe_ids(), ids);
            assert_eq!(editor.points(), before);
            editor.event(&open, &mut ctx);
            if !keyboard {
                editor.event(
                    &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
                    &mut ctx,
                );
            }
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx,
            );
            assert_eq!(actions.borrow().as_slice(), &[Action::Paste]);
            assert_eq!(editor.selected_keyframe_ids(), ids);
            assert_eq!(editor.points(), before);
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
                &mut ctx,
            );
            assert_eq!(editor.selected_keyframe_ids(), [ids[0]]);
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::F10, modifiers: Modifiers::shift() },
                &mut ctx,
            );
            assert_eq!(
                requests.borrow().last(),
                Some(&vec![ids[0]]),
                "singleton shares the lazy factory"
            );
            editor.event(
                &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                &mut ctx,
            );
            editor.event(
                &UiEvent::MouseDown {
                    position: editor.to_screen(editor.points()[1]),
                    button: MouseButton::Right,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            );
            assert_eq!(editor.selected_keyframe_ids(), [ids[1]]);
            assert_eq!(
                requests.borrow().last(),
                Some(&vec![ids[1]]),
                "unselected right-click addresses that key"
            );
            assert_eq!(actions.borrow().as_slice(), &[Action::Paste]);
            assert_eq!(editor.points(), before);
        }
    }

    #[test]
    fn pointer_draft_refuses_context_menu_until_cancelled() {
        let ids = [
            mondrian_core::KeyframeId::new(),
            mondrian_core::KeyframeId::new(),
            mondrian_core::KeyframeId::new(),
        ];
        let requests = std::rc::Rc::new(RefCell::new(Vec::new()));
        let menu_requests = requests.clone();
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.2),
            CurvePoint::new(0.5, 0.7),
            CurvePoint::new(1.0, 0.3),
        ])
        .with_keyframe_ids(ids.map(Some).to_vec())
        .with_point_context_menus(vec![
            None,
            Some(vec![MenuItem::new("Auto", Action::Paste)]),
            None,
        ])
        .with_selection_context_menu(move |keys| {
            menu_requests.borrow_mut().push(keys.to_vec());
            vec![MenuItem::new("Auto", Action::Paste)]
        });
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let origin = editor.points().to_vec();
        let point = editor.to_screen(editor.points()[1]);
        let mut focus = CurveFocus(Some(editor.id()));
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        editor.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        editor.event(
            &UiEvent::MouseDown {
                position: point,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(editor.has_active_pointer_interaction());
        editor.event(
            &UiEvent::MouseDown {
                position: point,
                button: MouseButton::Right,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::F10, modifiers: Modifiers::shift() },
            &mut ctx,
        );
        assert!(editor.context_menu.is_none());
        assert!(requests.borrow().is_empty());
        assert!(actions.borrow().is_empty());
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(!editor.has_active_pointer_interaction());
        assert_eq!(editor.points(), origin);
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        editor.event(
            &UiEvent::KeyDown { key: KeyCode::F10, modifiers: Modifiers::shift() },
            &mut ctx,
        );
        assert_eq!(*requests.borrow(), vec![ids.to_vec()]);
        assert!(editor.context_menu.is_some());
    }
}
