//! One semantic policy for combining an Effect result with its input.
use crate::BlendMode;
use serde::{Deserialize, Serialize};

/// Effect amount interpolation or an explicit image compositing operation.
/// Ordinary foreground layers and graph Blend nodes explicitly use `Blend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectPassBlendPolicy {
    /// Interpolate associated RGB and alpha, retaining the declared image domain.
    Mix,
    /// Composite the result using an explicitly selected blend law.
    Blend(BlendMode),
}
impl EffectPassBlendPolicy {
    /// Lower an already resolved canonical Adjustment mode once in the plan.
    /// Normal Adjustments mix the transformed lower image with its original.
    pub const fn from_adjustment_mode(mode: BlendMode) -> Self {
        match mode {
            BlendMode::Normal => Self::Mix,
            mode => Self::Blend(mode),
        }
    }
}
