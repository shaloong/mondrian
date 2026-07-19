//! Preview-resolution scale presentation helpers for UI adapters.

pub(crate) use crate::app::preview_quality::normalize_preview_resolution_scale;

pub(crate) fn preview_scale_percent_label(scale: f32) -> String {
    let percent = normalize_preview_resolution_scale(scale) * 100.0;
    let rounded = percent.round();
    if (percent - rounded).abs() <= f32::EPSILON {
        format!("{rounded:.0}%")
    } else {
        format!("{percent:.1}%")
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_preview_resolution_scale, preview_scale_percent_label};

    #[test]
    fn preview_scale_helpers_clamp_and_format_labels() {
        assert_eq!(normalize_preview_resolution_scale(0.0), 0.125);
        assert_eq!(normalize_preview_resolution_scale(2.0), 1.0);
        assert_eq!(normalize_preview_resolution_scale(f32::NAN), 0.5);
        assert_eq!(preview_scale_percent_label(0.0), "12.5%");
        assert_eq!(preview_scale_percent_label(0.5), "50%");
        assert_eq!(preview_scale_percent_label(1.0), "100%");
    }
}
