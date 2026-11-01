//! Echo: sample the effect input at earlier instants and composite the trail.
//!
//! This module owns no pixel kernel. An echo is a graph shape, not a pixel
//! operation: every tap is a [`crate::EffectRenderOp::TemporalFrameBlend`] that
//! samples the *pre-effect* value at `echo_time * index` (a signed offset, so
//! the default `-0.03333` seconds walks one frame into the past at 30 fps), and
//! the taps are composited over the current frame with one [`BlendMode`] and a
//! decaying opacity.
//!
//! The grade is deliberately small and total: it validates the authored values,
//! derives one tap per echo, and drops taps whose opacity rounds to nothing. The
//! graph builder walks [`EchoGrade::taps`], so the number of temporal samples in
//! the compiled graph is exactly the number of taps this module reports.
//!
//! # Contract
//!
//! `TemporalFrameBlend` is admitted only by the finite temporal executor (see
//! `crates/mondrian-effects/src/temporal_execution.rs`), so an echo graph is a
//! CPU evaluation: there is no single-frame GPU path for a value that reads
//! another instant. Declaring anything else here would promise pixels the
//! renderer cannot produce.
//!
//! # Limits
//!
//! The authoring range allows 30000 echoes because that is what the effect list
//! documents, but a graph carries one node per tap. [`ECHO_MAX_TAPS`] is the
//! implemented ceiling; a larger count fails closed instead of building a graph
//! that would exhaust the temporal program budget.

use mondrian_core::{BlendMode, TimelineTime, TimelineTimeError};

/// Fractional second resolution used to quantize a tap offset.
///
/// Echo times are authored as seconds and sampled as exact rational instants.
/// One microsecond keeps every authored value representable without dragging a
/// composition's timebase into an effect definition.
pub const ECHO_TIMESCALE: u32 = 1_000_000;

/// Authoring ceiling for the echo count (the effect list documents 30000).
pub const ECHO_AUTHOR_MAX_COUNT: u32 = 30_000;

/// Implemented ceiling for one echo graph.
pub const ECHO_MAX_TAPS: u32 = 32;

/// Authoring ceiling for the magnitude of one echo step, in seconds.
pub const ECHO_MAX_TIME_SECONDS: f32 = 30_000.0;

/// Authoring ceiling for the decay percentage.
pub const ECHO_MAX_DECAY_PERCENT: f32 = 100.0;

/// Authored default for the echo step, in seconds (one frame at 30 fps).
pub const ECHO_DEFAULT_TIME_SECONDS: f32 = -0.033_333_33;

/// Authored default for the echo count.
pub const ECHO_DEFAULT_COUNT: u32 = 1;

/// Authored default for the opacity of the first echo.
pub const ECHO_DEFAULT_STARTING_INTENSITY: f32 = 1.0;

/// Authored default for the per-echo fade-out percentage.
pub const ECHO_DEFAULT_DECAY_PERCENT: f32 = 1.0;

/// Number of operator menu entries.
pub const ECHO_OPERATOR_COUNT: usize = 7;

/// How one tap composites over the accumulated trail.
///
/// The order matches the authored menu (1-based) so a stored project keeps its
/// meaning when a new operator is appended at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EchoOperator {
    /// Linear dodge: the two colours are added.
    Add,
    /// Screen: the inverse multiplies.
    Screen,
    /// Multiply: the colours are multiplied.
    Multiply,
    /// Overlay: multiply below half, screen above.
    Overlay,
    /// Darken: the smaller channel wins.
    Darken,
    /// Lighten: the larger channel wins.
    Lighten,
    /// Normal: the tap replaces the pixel by its opacity.
    Normal,
}

impl EchoOperator {
    /// Stable authoring key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Screen => "screen",
            Self::Multiply => "multiply",
            Self::Overlay => "overlay",
            Self::Darken => "darken",
            Self::Lighten => "lighten",
            Self::Normal => "normal",
        }
    }

    /// Resolve a stored authoring key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|operator| operator.as_key() == key)
    }

    /// One-based authored menu index.
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Zero-based index used by serialized menus and the compositor.
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Add => 0,
            Self::Screen => 1,
            Self::Multiply => 2,
            Self::Overlay => 3,
            Self::Darken => 4,
            Self::Lighten => 5,
            Self::Normal => 6,
        }
    }

    /// Resolve one authored (one-based) menu value.
    pub fn from_menu(menu: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|operator| operator.as_menu() == menu)
    }

    /// Chinese label shown in the parameter menu.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Add => "相加",
            Self::Screen => "滤色",
            Self::Multiply => "正片叠底",
            Self::Overlay => "叠加",
            Self::Darken => "变暗",
            Self::Lighten => "变亮",
            Self::Normal => "正常",
        }
    }

    /// Composite mode applied to one tap.
    pub const fn blend_mode(self) -> BlendMode {
        match self {
            Self::Add => BlendMode::LinearDodge,
            Self::Screen => BlendMode::Screen,
            Self::Multiply => BlendMode::Multiply,
            Self::Overlay => BlendMode::Overlay,
            Self::Darken => BlendMode::Darken,
            Self::Lighten => BlendMode::Lighten,
            Self::Normal => BlendMode::Normal,
        }
    }

    /// Every operator in authored order.
    pub const ALL: [Self; ECHO_OPERATOR_COUNT] = [
        Self::Add,
        Self::Screen,
        Self::Multiply,
        Self::Overlay,
        Self::Darken,
        Self::Lighten,
        Self::Normal,
    ];
}

/// Why an echoed grade could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoError {
    /// The step is not a finite duration inside the authored range.
    EchoTime,
    /// The count exceeds what one graph can carry.
    Count,
    /// The starting intensity is not a finite value inside `[0, 1]`.
    StartingIntensity,
    /// The decay is not a finite percentage inside `[0, 100]`.
    Decay,
}

impl std::fmt::Display for EchoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::EchoTime => "echo time must be a finite value inside [-30000, 30000] seconds",
            Self::Count => "echo count must not exceed 32 taps in one graph",
            Self::StartingIntensity => {
                "echo starting intensity must be a finite value inside [0, 1]"
            }
            Self::Decay => "echo decay must be a finite percentage inside [0, 100]",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for EchoError {}

/// One temporal tap of an echo graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EchoTap {
    index: u32,
    offset: TimelineTime,
    weight: f32,
}

impl EchoTap {
    /// One-based echo index, matching the authored count.
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Signed instant offset added to the current time.
    pub const fn offset(self) -> TimelineTime {
        self.offset
    }

    /// Composited opacity of this tap.
    pub const fn weight(self) -> f32 {
        self.weight
    }
}

/// Validated echo authoring state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EchoGrade {
    echo_time: f32,
    count: u32,
    starting_intensity: f32,
    decay_percent: f32,
    operator: EchoOperator,
}

impl EchoGrade {
    /// Validate one authored echo.
    pub fn new(
        echo_time: f32,
        count: u32,
        starting_intensity: f32,
        decay_percent: f32,
        operator: EchoOperator,
    ) -> Result<Self, EchoError> {
        if !echo_time.is_finite() || echo_time.abs() > ECHO_MAX_TIME_SECONDS {
            return Err(EchoError::EchoTime);
        }
        if count > ECHO_MAX_TAPS {
            return Err(EchoError::Count);
        }
        if !starting_intensity.is_finite() || !(0.0..=1.0).contains(&starting_intensity) {
            return Err(EchoError::StartingIntensity);
        }
        if !decay_percent.is_finite() || !(0.0..=ECHO_MAX_DECAY_PERCENT).contains(&decay_percent) {
            return Err(EchoError::Decay);
        }
        Ok(Self {
            echo_time,
            count,
            starting_intensity,
            decay_percent,
            operator,
        })
    }

    /// Authored step between two echoes, in seconds.
    pub const fn echo_time(self) -> f32 {
        self.echo_time
    }

    /// Authored echo count.
    pub const fn count(self) -> u32 {
        self.count
    }

    /// Authored opacity of the first echo.
    pub const fn starting_intensity(self) -> f32 {
        self.starting_intensity
    }

    /// Authored decay percentage applied to every later echo.
    pub const fn decay_percent(self) -> f32 {
        self.decay_percent
    }

    /// Compositing operator applied to every tap.
    pub const fn operator(self) -> EchoOperator {
        self.operator
    }

    /// Whether the authored state produces no visible trail.
    pub fn is_identity(self) -> bool {
        self.taps().is_empty()
    }

    /// Opacity of one (one-based) echo.
    ///
    /// `decay` is the percentage of opacity each echo loses relative to the one
    /// before it, which is why the documented default `1` leaves a long, gently
    /// fading trail instead of dropping it: `weight(k) = starting * (1 - decay/100)^(k-1)`.
    /// A decay of `100` therefore keeps only the first echo, and `0` keeps them
    /// all at equal opacity.
    pub fn weight(self, index: u32) -> f32 {
        if index == 0 {
            return 0.0;
        }
        let retained = 1.0 - self.decay_percent / ECHO_MAX_DECAY_PERCENT;
        self.starting_intensity * retained.powi(i32::try_from(index - 1).unwrap_or(i32::MAX))
    }

    /// Exact instants sampled by this echo, in authored order.
    ///
    /// A tap whose opacity is not representable is dropped: compositing nothing
    /// would still cost a temporal sample, a texture and one blend.
    pub fn taps(self) -> Vec<EchoTap> {
        if self.echo_time == 0.0 || self.count == 0 || self.starting_intensity <= 0.0 {
            return Vec::new();
        }
        let mut taps = Vec::with_capacity(self.count as usize);
        for index in 1..=self.count {
            let weight = self.weight(index);
            if weight <= 0.0 {
                break;
            }
            let seconds = f64::from(self.echo_time) * f64::from(index);
            match TimelineTime::from_f64_quantized(seconds, ECHO_TIMESCALE) {
                Ok(offset) if !offset.is_zero() => {
                    taps.push(EchoTap { index, offset, weight });
                }
                Ok(_) => break,
                Err(_) => break,
            }
        }
        taps
    }
}

/// Quantize one signed instant offset, exposed for the graph builder's tests.
pub fn echo_offset(seconds: f64) -> Result<TimelineTime, TimelineTimeError> {
    TimelineTime::from_f64_quantized(seconds, ECHO_TIMESCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_menu_round_trips_in_authored_order() {
        for (index, operator) in EchoOperator::ALL.into_iter().enumerate() {
            assert_eq!(operator.as_menu(), index as u32 + 1);
            assert_eq!(EchoOperator::from_menu(operator.as_menu()), Some(operator));
            assert_eq!(EchoOperator::from_key(operator.as_key()), Some(operator));
        }
        assert_eq!(EchoOperator::from_menu(0), None);
        assert_eq!(EchoOperator::from_menu(8), None);
    }

    #[test]
    fn the_trail_decays_from_the_starting_intensity() {
        let grade = EchoGrade::new(-0.04, 3, 1.0, 50.0, EchoOperator::Screen).expect("grade");
        let taps = grade.taps();
        assert_eq!(taps.len(), 3);
        assert_eq!(taps[0].weight(), 1.0);
        assert_eq!(taps[1].weight(), 0.5);
        assert_eq!(taps[2].weight(), 0.25);
        assert_eq!(taps[0].offset(), echo_offset(-0.04).expect("offset"));
        assert_eq!(taps[1].offset(), echo_offset(-0.08).expect("offset"));
        assert_eq!(taps[2].offset(), echo_offset(-0.12).expect("offset"));
    }

    #[test]
    fn a_zero_step_or_zero_count_is_identity() {
        let still = EchoGrade::new(0.0, 4, 1.0, 100.0, EchoOperator::Add).expect("grade");
        assert!(still.is_identity());
        let none = EchoGrade::new(-0.04, 0, 1.0, 100.0, EchoOperator::Add).expect("grade");
        assert!(none.is_identity());
    }

    #[test]
    fn a_full_decay_keeps_only_the_first_tap() {
        let grade = EchoGrade::new(-0.04, 5, 1.0, 100.0, EchoOperator::Multiply).expect("grade");
        let taps = grade.taps();
        assert_eq!(taps.len(), 1);
        assert_eq!(taps[0].weight(), 1.0);
    }

    #[test]
    fn a_zero_decay_keeps_every_tap_at_full_opacity() {
        let grade = EchoGrade::new(-0.04, 3, 1.0, 0.0, EchoOperator::Add).expect("grade");
        let taps = grade.taps();
        assert_eq!(taps.len(), 3);
        assert!(taps.iter().all(|tap| tap.weight() == 1.0));
    }

    #[test]
    fn the_documented_default_leaves_a_long_trail() {
        let grade = EchoGrade::new(
            ECHO_DEFAULT_TIME_SECONDS,
            4,
            ECHO_DEFAULT_STARTING_INTENSITY,
            ECHO_DEFAULT_DECAY_PERCENT,
            EchoOperator::Add,
        )
        .expect("grade");
        let taps = grade.taps();
        assert_eq!(taps.len(), 4);
        assert!(
            (taps[3].weight() - 0.970_299).abs() < 1e-6,
            "{}",
            taps[3].weight()
        );
    }

    #[test]
    fn a_zero_starting_intensity_has_no_trail() {
        let grade = EchoGrade::new(-0.04, 3, 0.0, 50.0, EchoOperator::Add).expect("grade");
        assert!(grade.is_identity());
    }

    #[test]
    fn future_steps_are_preserved_instead_of_being_mirrored() {
        let grade = EchoGrade::new(0.04, 2, 1.0, 50.0, EchoOperator::Add).expect("grade");
        let taps = grade.taps();
        assert_eq!(taps[0].offset(), echo_offset(0.04).expect("offset"));
        assert_eq!(taps[1].offset(), echo_offset(0.08).expect("offset"));
    }

    #[test]
    fn the_count_ceiling_fails_closed() {
        assert_eq!(
            EchoGrade::new(-0.04, ECHO_MAX_TAPS + 1, 1.0, 50.0, EchoOperator::Add).err(),
            Some(EchoError::Count)
        );
        assert!(
            EchoGrade::new(-0.04, ECHO_AUTHOR_MAX_COUNT, 1.0, 50.0, EchoOperator::Add).is_err()
        );
        assert!(EchoGrade::new(-0.04, ECHO_MAX_TAPS, 1.0, 50.0, EchoOperator::Add).is_ok());
    }

    #[test]
    fn every_authored_range_is_enforced() {
        assert_eq!(
            EchoGrade::new(f32::NAN, 1, 1.0, 50.0, EchoOperator::Add).err(),
            Some(EchoError::EchoTime)
        );
        assert_eq!(
            EchoGrade::new(ECHO_MAX_TIME_SECONDS + 1.0, 1, 1.0, 50.0, EchoOperator::Add).err(),
            Some(EchoError::EchoTime)
        );
        assert_eq!(
            EchoGrade::new(-0.04, 1, 1.5, 50.0, EchoOperator::Add).err(),
            Some(EchoError::StartingIntensity)
        );
        assert_eq!(
            EchoGrade::new(-0.04, 1, 1.0, 101.0, EchoOperator::Add).err(),
            Some(EchoError::Decay)
        );
    }

    #[test]
    fn operators_map_to_the_documented_blend_modes() {
        assert_eq!(EchoOperator::Add.blend_mode(), BlendMode::LinearDodge);
        assert_eq!(EchoOperator::Screen.blend_mode(), BlendMode::Screen);
        assert_eq!(EchoOperator::Multiply.blend_mode(), BlendMode::Multiply);
        assert_eq!(EchoOperator::Overlay.blend_mode(), BlendMode::Overlay);
        assert_eq!(EchoOperator::Darken.blend_mode(), BlendMode::Darken);
        assert_eq!(EchoOperator::Lighten.blend_mode(), BlendMode::Lighten);
        assert_eq!(EchoOperator::Normal.blend_mode(), BlendMode::Normal);
    }
}
