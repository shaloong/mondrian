//! UI-independent preview quality normalization.
//!
//! Author mutations, Window presentation, and Headless execution must resolve
//! the same legal runtime scale instead of maintaining parallel clamp rules.

/// Normalize an authored preview resolution scale for execution.
pub(crate) fn normalize_preview_resolution_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(0.125, 1.0)
    } else {
        0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_has_one_bounded_fail_safe_contract() {
        assert_eq!(normalize_preview_resolution_scale(0.0), 0.125);
        assert_eq!(normalize_preview_resolution_scale(0.25), 0.25);
        assert_eq!(normalize_preview_resolution_scale(2.0), 1.0);
        assert_eq!(normalize_preview_resolution_scale(f32::NAN), 0.5);
        assert_eq!(normalize_preview_resolution_scale(f32::INFINITY), 0.5);
    }
}
