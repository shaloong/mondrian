//! Shared unbounded HSL helpers used by the channel and hue effects.
//!
//! Every helper works on unclamped working-space RGB triples: hues are derived
//! from the largest and smallest channel, chroma is maintained exactly, and
//! reconstruction adds the lightness offset back instead of clamping. That keeps
//! extended-range and negative channels intact for effects that must not crush
//! scene-linear values.

/// Chroma below which a pixel is treated as neutral and carries no hue.
pub(crate) const NEUTRAL_CHROMA: f32 = 1.0e-6;

/// Half a turn expressed in turns, used to rotate a hue to its opposite.
pub(crate) const HALF_TURN: f32 = 0.5;

/// Largest and smallest channel of an RGB triple.
pub(crate) const fn extremes(rgb: [f32; 3]) -> (f32, f32) {
    let high = if rgb[0] >= rgb[1] && rgb[0] >= rgb[2] {
        rgb[0]
    } else if rgb[1] >= rgb[2] {
        rgb[1]
    } else {
        rgb[2]
    };
    let low = if rgb[0] <= rgb[1] && rgb[0] <= rgb[2] {
        rgb[0]
    } else if rgb[1] <= rgb[2] {
        rgb[1]
    } else {
        rgb[2]
    };
    (high, low)
}

/// Hue of a chromatic RGB triple in `[0, 1)` turns.
///
/// `high` and `chroma` must be the values produced by [`extremes`]; callers
/// guard against neutral pixels before calling this.
pub(crate) fn hue_turns(rgb: [f32; 3], high: f32, chroma: f32) -> f32 {
    let sextant = if high == rgb[0] {
        (rgb[1] - rgb[2]) / chroma
    } else if high == rgb[1] {
        (rgb[2] - rgb[0]) / chroma + 2.0
    } else {
        (rgb[0] - rgb[1]) / chroma + 4.0
    };
    (sextant / 6.0).rem_euclid(1.0)
}

/// Rebuild an RGB triple from hue (turns), chroma, and lightness.
///
/// The reconstruction is exact for any lightness: the offset is
/// `lightness - chroma / 2`, so `high - low` stays equal to `chroma` and the
/// mean of `high` and `low` stays equal to `lightness`.
pub(crate) fn from_hue_chroma_lightness(hue_turns: f32, chroma: f32, lightness: f32) -> [f32; 3] {
    let sextant = hue_turns.rem_euclid(1.0) * 6.0;
    let second = chroma * (1.0 - (sextant.rem_euclid(2.0) - 1.0).abs());
    let primary = match sextant.floor() as i32 {
        0 => [chroma, second, 0.0],
        1 => [second, chroma, 0.0],
        2 => [0.0, chroma, second],
        3 => [0.0, second, chroma],
        4 => [second, 0.0, chroma],
        _ => [chroma, 0.0, second],
    };
    let offset = lightness - chroma * 0.5;
    [
        primary[0] + offset,
        primary[1] + offset,
        primary[2] + offset,
    ]
}

/// Distance between two hues in turns, wrapped into `[0, 0.5]`.
pub(crate) fn hue_distance_turns(a: f32, b: f32) -> f32 {
    let difference = (a - b).rem_euclid(1.0);
    if difference > HALF_TURN {
        1.0 - difference
    } else {
        difference
    }
}

/// Hermite interpolation of `x` between `edge0` and `edge1`, clamped outside.
pub(crate) fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rotate the hue of a chromatic triple by half a turn, keeping chroma and lightness.
pub(crate) fn opposite_hue(rgb: [f32; 3]) -> [f32; 3] {
    let (high, low) = extremes(rgb);
    let chroma = high - low;
    if chroma <= NEUTRAL_CHROMA {
        return rgb;
    }
    let hue = hue_turns(rgb, high, chroma);
    from_hue_chroma_lightness(hue + HALF_TURN, chroma, (high + low) * 0.5)
}

/// Mirror the chroma of a triple around its lightness, keeping hue and lightness.
pub(crate) fn opposite_chroma(rgb: [f32; 3]) -> [f32; 3] {
    let (high, low) = extremes(rgb);
    let chroma = high - low;
    if chroma <= NEUTRAL_CHROMA {
        return rgb;
    }
    let lightness = (high + low) * 0.5;
    let range = 1.0 - (2.0 * lightness - 1.0).abs();
    if range <= NEUTRAL_CHROMA || chroma > range {
        return rgb;
    }
    let scale = (range - chroma) / chroma;
    [
        lightness + (rgb[0] - lightness) * scale,
        lightness + (rgb[1] - lightness) * scale,
        lightness + (rgb[2] - lightness) * scale,
    ]
}

/// Reflect the lightness of a triple around `0.5`, keeping its chroma.
///
/// The move is a common channel offset, so it stays exact for extended-range
/// pixels and leaves neutral pixels reflecting with the others.
pub(crate) fn opposite_lightness(rgb: [f32; 3]) -> [f32; 3] {
    let (high, low) = extremes(rgb);
    let shift = 1.0 - (high + low);
    [rgb[0] + shift, rgb[1] + shift, rgb[2] + shift]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extremes_and_hue_agree_with_the_channel_order() {
        assert_eq!(extremes([0.2, 0.9, 0.4]), (0.9, 0.2));
        let red = [0.9, 0.2, 0.2];
        let green = [0.2, 0.9, 0.2];
        let blue = [0.2, 0.2, 0.9];
        let yellow = [0.9, 0.9, 0.2];
        for (pixel, expected) in [
            (red, 0.0),
            (green, 1.0 / 3.0),
            (blue, 2.0 / 3.0),
            (yellow, 1.0 / 6.0),
        ] {
            let (high, low) = extremes(pixel);
            let hue = hue_turns(pixel, high, high - low);
            assert!((hue - expected).abs() < 1.0e-6, "{pixel:?} hue {hue}");
        }
    }

    #[test]
    fn reconstruction_keeps_chroma_and_lightness_for_extended_range() {
        let rebuilt = from_hue_chroma_lightness(0.5, 3.0, 2.0);
        let (high, low) = extremes(rebuilt);
        assert!((high - low - 3.0).abs() < 1.0e-6);
        assert!(((high + low) * 0.5 - 2.0).abs() < 1.0e-6);
    }

    #[test]
    fn hue_distance_wraps_across_the_turn() {
        assert!((hue_distance_turns(0.05, 0.95) - 0.1).abs() < 1.0e-6);
        assert!((hue_distance_turns(0.0, 0.5) - 0.5).abs() < 1.0e-6);
        assert_eq!(hue_distance_turns(0.25, 0.25), 0.0);
    }

    #[test]
    fn smoothstep_is_clamped_and_monotone() {
        assert_eq!(smoothstep(0.0, 1.0, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
        assert!((smoothstep(0.0, 1.0, 0.5) - 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn neutral_pixels_are_left_alone_by_both_opposites() {
        assert_eq!(opposite_hue([0.4, 0.4, 0.4]), [0.4, 0.4, 0.4]);
        assert_eq!(opposite_chroma([0.4, 0.4, 0.4]), [0.4, 0.4, 0.4]);
    }
}
