//! Native display color-management probes.

#[cfg(any(target_os = "linux", test))]
pub(crate) mod edid;

#[cfg(target_os = "linux")]
pub(crate) mod linux;

#[cfg(target_os = "macos")]
pub(crate) mod macos;

#[cfg(target_os = "windows")]
pub(crate) mod windows;

use mondrian_platform_core::DisplayProfileProbeTarget;

fn physical_rect_matches_target(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    target: DisplayProfileProbeTarget,
) -> bool {
    if !target.is_valid() || width == 0 || height == 0 {
        return false;
    }
    if x == target.x && y == target.y && width == target.width && height == target.height {
        return true;
    }
    let center_x = target.x.saturating_add((target.width / 2) as i32);
    let center_y = target.y.saturating_add((target.height / 2) as i32);
    center_x >= x
        && center_y >= y
        && center_x < x.saturating_add(width as i32)
        && center_y < y.saturating_add(height as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_monitor_match_supports_retina_pixels_and_negative_coordinates() {
        let retina = DisplayProfileProbeTarget::new((0, 0), (3_456, 2_234));
        assert!(physical_rect_matches_target(0, 0, 3_456, 2_234, retina));

        let left = DisplayProfileProbeTarget::new((-1_920, 80), (1_920, 1_080));
        assert!(physical_rect_matches_target(-1_920, 80, 1_920, 1_080, left));
        assert!(!physical_rect_matches_target(0, 0, 3_456, 2_234, left));
    }

    #[test]
    fn empty_or_detached_targets_never_match_the_primary_display() {
        let empty = DisplayProfileProbeTarget::new((0, 0), (0, 0));
        assert!(!physical_rect_matches_target(0, 0, 1_920, 1_080, empty));
    }
}
