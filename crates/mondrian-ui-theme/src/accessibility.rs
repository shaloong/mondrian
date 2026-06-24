//! Accessibility-oriented theme derivation.
//!
//! Product shells map platform or user preferences into this small contract;
//! widgets keep consuming semantic theme tokens instead of branching on
//! accessibility modes themselves.

use crate::Theme;

const MIN_TEXT_SCALE: f32 = 0.85;
const MAX_TEXT_SCALE: f32 = 1.6;
const DEFAULT_TEXT_SCALE: f32 = 1.0;

/// User or platform accessibility preferences that can derive a theme variant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccessibilityPreferences {
    /// Strengthen text, borders, focus rings, guides, and similar contrast-critical tokens.
    pub high_contrast: bool,
    /// Scale typography tokens while preserving each style's line-height ratio.
    pub text_scale: f32,
    /// Disable nonessential theme animation durations.
    pub reduced_motion: bool,
}

impl Default for AccessibilityPreferences {
    fn default() -> Self {
        Self {
            high_contrast: false,
            text_scale: DEFAULT_TEXT_SCALE,
            reduced_motion: false,
        }
    }
}

impl AccessibilityPreferences {
    /// Enable or disable high-contrast theme derivation.
    pub fn with_high_contrast(mut self, high_contrast: bool) -> Self {
        self.high_contrast = high_contrast;
        self
    }

    /// Set the requested text scale.
    ///
    /// The value is clamped when applied so accidental extreme preferences do
    /// not collapse dense editor layouts.
    pub fn with_text_scale(mut self, text_scale: f32) -> Self {
        self.text_scale = text_scale;
        self
    }

    /// Enable or disable reduced motion theme derivation.
    pub fn with_reduced_motion(mut self, reduced_motion: bool) -> Self {
        self.reduced_motion = reduced_motion;
        self
    }

    /// Return a finite text scale inside the supported editor range.
    pub fn normalized_text_scale(self) -> f32 {
        if self.text_scale.is_finite() {
            self.text_scale.clamp(MIN_TEXT_SCALE, MAX_TEXT_SCALE)
        } else {
            DEFAULT_TEXT_SCALE
        }
    }
}

impl Theme {
    /// Return a theme variant derived from accessibility preferences.
    pub fn with_accessibility(mut self, preferences: AccessibilityPreferences) -> Self {
        self.apply_accessibility(preferences);
        self
    }

    /// Apply accessibility preferences to this theme in place.
    pub fn apply_accessibility(&mut self, preferences: AccessibilityPreferences) {
        if preferences.high_contrast {
            self.colors = self.colors.clone().with_high_contrast();
        }
        let text_scale = preferences.normalized_text_scale();
        if (text_scale - DEFAULT_TEXT_SCALE).abs() > f32::EPSILON {
            self.typography = self.typography.scaled(text_scale);
        }
        if preferences.reduced_motion {
            self.spacing = self.spacing.clone().with_reduced_motion();
        }
    }
}
