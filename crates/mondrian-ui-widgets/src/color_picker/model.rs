use mondrian_core::{CmykColor, Color, HslColor, HsvColor, RgbaColor};

use super::ColorPickerMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ColorField {
    Hex,
    R,
    G,
    B,
    A,
    H,
    S,
    L,
    V,
    C,
    M,
    Y,
    K,
}

impl ColorField {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Hex => "Hex",
            Self::R => "R",
            Self::G => "G",
            Self::B => "B",
            Self::A => "A",
            Self::H => "H",
            Self::S => "S",
            Self::L => "L",
            Self::V => "V",
            Self::C => "C",
            Self::M => "M",
            Self::Y => "Y",
            Self::K => "K",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ColorFieldApply {
    pub(super) color: Color,
    pub(super) hue: f32,
    pub(super) color_changed: bool,
}

pub(super) fn field_text(color: Color, mode: ColorPickerMode, field: ColorField) -> String {
    match field {
        ColorField::Hex => color.to_hex_rgba(),
        ColorField::R => int_channel(color.r).to_string(),
        ColorField::G => int_channel(color.g).to_string(),
        ColorField::B => int_channel(color.b).to_string(),
        ColorField::A => percent_channel(color.a).to_string(),
        ColorField::H => match mode {
            ColorPickerMode::Hsl => format_number(color.to_hsl().h),
            ColorPickerMode::Hsv => format_number(color.to_hsv().h),
            _ => "0".into(),
        },
        ColorField::S => match mode {
            ColorPickerMode::Hsl => percent_channel(color.to_hsl().s).to_string(),
            ColorPickerMode::Hsv => percent_channel(color.to_hsv().s).to_string(),
            _ => "0".into(),
        },
        ColorField::L => percent_channel(color.to_hsl().l).to_string(),
        ColorField::V => percent_channel(color.to_hsv().v).to_string(),
        ColorField::C => percent_channel(color.to_cmyk().c).to_string(),
        ColorField::M => percent_channel(color.to_cmyk().m).to_string(),
        ColorField::Y => percent_channel(color.to_cmyk().y).to_string(),
        ColorField::K => percent_channel(color.to_cmyk().k).to_string(),
    }
}

pub(super) fn hue_for_color(current_hue: f32, color: Color) -> f32 {
    let hsv = color.to_hsv();
    if hsv.s > 0.001 && hsv.v > 0.001 {
        hsv.h
    } else {
        current_hue
    }
}

pub(super) fn apply_field_texts<'a>(
    old_color: Color,
    old_hue: f32,
    mode: ColorPickerMode,
    active_fields: &[ColorField],
    text: impl Fn(usize) -> &'a str,
) -> Option<ColorFieldApply> {
    let color = match mode {
        ColorPickerMode::Hex => Color::parse_hex(text(0)).ok()?,
        ColorPickerMode::Rgb => {
            let r = parse_u8_channel(text(0))?;
            let g = parse_u8_channel(text(1))?;
            let b = parse_u8_channel(text(2))?;
            let a = parse_percent_channel(text(3))?;
            Color::from_rgba(RgbaColor { r, g, b, a })
        }
        ColorPickerMode::Hsl => {
            let h = parse_hue(text(0))?;
            let s = parse_percent_channel(text(1))?;
            let l = parse_percent_channel(text(2))?;
            let a = parse_percent_channel(text(3))?;
            Color::from_hsl(HslColor { h, s, l, a })
        }
        ColorPickerMode::Hsv => {
            let h = parse_hue(text(0))?;
            let s = parse_percent_channel(text(1))?;
            let v = parse_percent_channel(text(2))?;
            let a = parse_percent_channel(text(3))?;
            Color::from_hsv(HsvColor { h, s, v, a })
        }
        ColorPickerMode::Cmyk => {
            let c = parse_percent_channel(text(0))?;
            let m = parse_percent_channel(text(1))?;
            let y = parse_percent_channel(text(2))?;
            let k = parse_percent_channel(text(3))?;
            let a = parse_percent_channel(text(4))?;
            Color::from_cmyk(CmykColor { c, m, y, k, a })
        }
    };

    if active_fields.is_empty() {
        return None;
    }

    let hue = match mode {
        ColorPickerMode::Hsl | ColorPickerMode::Hsv => parse_hue(text(0)).unwrap_or(old_hue),
        _ => hue_for_color(old_hue, color),
    };
    Some(ColorFieldApply { color, hue, color_changed: color != old_color })
}

fn parse_u8_channel(input: &str) -> Option<f32> {
    let value = input.trim().parse::<f32>().ok()?;
    Some((value.round() / 255.0).clamp(0.0, 1.0))
}

fn parse_percent_channel(input: &str) -> Option<f32> {
    let value = input.trim().trim_end_matches('%').parse::<f32>().ok()?;
    Some((value / 100.0).clamp(0.0, 1.0))
}

fn parse_hue(input: &str) -> Option<f32> {
    Some(input.trim().parse::<f32>().ok()?.rem_euclid(360.0))
}

fn int_channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn percent_channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 100.0).round() as u8
}

fn format_number(value: f32) -> String {
    if (value.round() - value).abs() <= 0.01 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 0.0001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn field_text_formats_hex_rgb_hsv_and_cmyk_channels() {
        let color = Color::from_rgba(RgbaColor { r: 0.2, g: 0.4, b: 0.6, a: 0.75 });

        assert_eq!(
            field_text(color, ColorPickerMode::Hex, ColorField::Hex),
            "#336699BF"
        );
        assert_eq!(field_text(color, ColorPickerMode::Rgb, ColorField::R), "51");
        assert_eq!(field_text(color, ColorPickerMode::Rgb, ColorField::A), "75");
        assert_eq!(
            field_text(color, ColorPickerMode::Hsv, ColorField::H),
            "210"
        );
        assert_eq!(
            field_text(color, ColorPickerMode::Cmyk, ColorField::K),
            "40"
        );
    }

    #[test]
    fn apply_field_texts_parses_and_clamps_rgb_channels() {
        let old = Color::from_rgba(RgbaColor { r: 0.0, g: 0.0, b: 0.0, a: 1.0 });
        let values = ["300", "-10", "128", "125"];

        let update = apply_field_texts(
            old,
            0.0,
            ColorPickerMode::Rgb,
            ColorPickerMode::Rgb.fields(),
            |index| values[index],
        )
        .expect("valid rgb");

        assert_close(update.color.r, 1.0);
        assert_close(update.color.g, 0.0);
        assert_close(update.color.b, 128.0 / 255.0);
        assert_close(update.color.a, 1.0);
        assert!(update.color_changed);
    }

    #[test]
    fn apply_field_texts_rejects_invalid_field_without_partial_update() {
        let old = Color::from_rgba(RgbaColor { r: 0.0, g: 0.0, b: 0.0, a: 1.0 });
        let values = ["120", "bad", "50", "100"];

        assert_eq!(
            apply_field_texts(
                old,
                42.0,
                ColorPickerMode::Hsl,
                ColorPickerMode::Hsl.fields(),
                |index| values[index],
            ),
            None
        );
    }

    #[test]
    fn apply_field_texts_preserves_explicit_hue_for_neutral_hsv_color() {
        let old = Color::from_hsv(HsvColor { h: 10.0, s: 0.0, v: 0.5, a: 1.0 });
        let values = ["270", "0", "50", "100"];

        let update = apply_field_texts(
            old,
            10.0,
            ColorPickerMode::Hsv,
            ColorPickerMode::Hsv.fields(),
            |index| values[index],
        )
        .expect("valid hsv");

        assert_close(update.hue, 270.0);
        assert!(!update.color_changed);
    }

    #[test]
    fn hue_for_color_keeps_previous_hue_for_black_or_gray() {
        let gray = Color::from_hsv(HsvColor { h: 120.0, s: 0.0, v: 0.5, a: 1.0 });
        assert_close(hue_for_color(33.0, gray), 33.0);

        let saturated = Color::from_hsv(HsvColor { h: 210.0, s: 0.5, v: 0.5, a: 1.0 });
        assert_close(hue_for_color(33.0, saturated), 210.0);
    }
}
