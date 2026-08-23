//! macOS display ICC and EDR probing through CoreGraphics/AppKit.

use mondrian_platform_core::{
    DisplayHdrProbeDetails, DisplayHdrProbeResult, DisplayIccProfileProbeResult,
    DisplayProbeBackend, DisplayProfileProbeTarget,
};
use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_core_graphics::{
    CGColorSpace, CGDirectDisplayID, CGDisplayBounds, CGDisplayCopyColorSpace, CGDisplayPixelsHigh,
    CGDisplayPixelsWide,
};

pub(crate) fn display_icc_profile(
    target: DisplayProfileProbeTarget,
) -> DisplayIccProfileProbeResult {
    let Some((display_id, display_name, _)) = screen_for_target(target) else {
        return DisplayIccProfileProbeResult::missing(
            DisplayProbeBackend::MacOsCoreGraphics,
            None,
            "no NSScreen matched the winit display rectangle, or probing was not on the AppKit main thread",
        );
    };

    let color_space = CGDisplayCopyColorSpace(display_id);
    match CGColorSpace::icc_data(Some(&color_space)) {
        Some(data) if !data.is_empty() => DisplayIccProfileProbeResult::found_bytes(
            DisplayProbeBackend::MacOsCoreGraphics,
            Some(display_name),
            data.to_vec(),
        ),
        _ => DisplayIccProfileProbeResult::missing(
            DisplayProbeBackend::MacOsCoreGraphics,
            Some(display_name),
            "CGDisplayCopyColorSpace returned no ICC payload for this display",
        ),
    }
}

pub(crate) fn display_hdr_state(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    let Some((display_id, display_name, screen)) = screen_for_target(target) else {
        return DisplayHdrProbeResult::missing(
            DisplayProbeBackend::MacOsAppKit,
            None,
            "no NSScreen matched the winit display rectangle, or probing was not on the AppKit main thread",
        );
    };

    let current = screen.maximumExtendedDynamicRangeColorComponentValue();
    let potential = screen.maximumPotentialExtendedDynamicRangeColorComponentValue();
    let reference = screen.maximumReferenceExtendedDynamicRangeColorComponentValue();
    let wide_color = CGDisplayCopyColorSpace(display_id).is_wide_gamut_rgb();
    let supported = potential.is_finite() && potential > 1.0;
    let enabled = (current.is_finite() && current > 1.0).then_some(true);

    DisplayHdrProbeResult::found(
        DisplayProbeBackend::MacOsAppKit,
        Some(display_name),
        DisplayHdrProbeDetails {
            hdr_supported: Some(supported),
            // Apple documents 1.0 when no onscreen content requests EDR, so
            // that value is not proof that EDR is disabled.
            hdr_enabled: enabled,
            wide_color_active: Some(wide_color),
            force_disabled: Some(false),
            current_headroom_ppm: headroom_ppm(current),
            potential_headroom_ppm: headroom_ppm(potential),
            reference_headroom_ppm: headroom_ppm(reference),
            ..DisplayHdrProbeDetails::default()
        },
    )
}

fn screen_for_target(
    target: DisplayProfileProbeTarget,
) -> Option<(CGDirectDisplayID, String, objc2::rc::Retained<NSScreen>)> {
    let mtm = MainThreadMarker::new()?;
    NSScreen::screens(mtm).to_vec().into_iter().find_map(|screen| {
        let display_id = screen.CGDirectDisplayID();
        screen_matches_target(display_id, screen.backingScaleFactor(), target)
            .then(|| (display_id, screen.localizedName().to_string(), screen))
    })
}

fn screen_matches_target(
    display_id: CGDirectDisplayID,
    scale_factor: f64,
    target: DisplayProfileProbeTarget,
) -> bool {
    // Match winit's macOS MonitorHandle contract: CoreGraphics display sizes
    // and bounds are logical screen coordinates converted by backing scale.
    let pixel_width = (CGDisplayPixelsWide(display_id) as f64 * scale_factor).round() as u32;
    let pixel_height = (CGDisplayPixelsHigh(display_id) as f64 * scale_factor).round() as u32;
    let bounds = CGDisplayBounds(display_id);
    let left = (bounds.origin.x * scale_factor).round() as i32;
    let top = (bounds.origin.y * scale_factor).round() as i32;
    let right = left.saturating_add((bounds.size.width * scale_factor).round() as i32);
    let bottom = top.saturating_add((bounds.size.height * scale_factor).round() as i32);
    let center_x = target.x.saturating_add((target.width / 2) as i32);
    let center_y = target.y.saturating_add((target.height / 2) as i32);

    (pixel_width == target.width && pixel_height == target.height)
        && ((left == target.x && top == target.y)
            || (center_x >= left && center_x < right && center_y >= top && center_y < bottom))
}

fn headroom_ppm(value: f64) -> Option<u32> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some((value * 1_000_000.0).round().clamp(0.0, f64::from(u32::MAX)) as u32)
}
