use super::{enum_options, required_enum, required_int, required_text, title_error};
use crate::automation::{
    ParameterInvalidValuePolicy, ParameterNumericContract, ParameterNumericRange,
    ParameterTextMode, ParameterUnit, PropertyBag, PropertyDescriptor, PropertyValue,
};
use crate::{
    BasicTitle, FrameRounding, ParameterId, Rational, Result, SmpteCountingMode,
    SmpteDisplayTimecodeContract, TimelineDisplayFormat, TimelineDisplaySettings, TimelineTime,
    BASIC_TITLE_MAX_TEXT_BYTES,
};

const DECIMAL_SCALE: i128 = 100_000_000;
const MAX_CONTROL: f64 = 1_000_000_000.0;
const LABEL_BYTES: usize = 128;

impl BasicTitle {
    pub const NUMBERS_FORMAT_PATH: &'static str = "title.numbers.format";
    pub const NUMBERS_START_PATH: &'static str = "title.numbers.start";
    pub const NUMBERS_STEP_PATH: &'static str = "title.numbers.step";
    pub const NUMBERS_DECIMALS_PATH: &'static str = "title.numbers.decimals";
    pub const NUMBERS_GROUP_PATH: &'static str = "title.numbers.group_thousands";
    pub const NUMBERS_PREFIX_PATH: &'static str = "title.numbers.prefix";
    pub const NUMBERS_SUFFIX_PATH: &'static str = "title.numbers.suffix";
    pub const NUMBERS_ZERO_PAD_PATH: &'static str = "title.numbers.zero_pad";

    pub const NUMBERS_PROPERTY_PATHS: [&'static str; 8] = [
        Self::NUMBERS_FORMAT_PATH,
        Self::NUMBERS_START_PATH,
        Self::NUMBERS_STEP_PATH,
        Self::NUMBERS_DECIMALS_PATH,
        Self::NUMBERS_GROUP_PATH,
        Self::NUMBERS_ZERO_PAD_PATH,
        Self::NUMBERS_PREFIX_PATH,
        Self::NUMBERS_SUFFIX_PATH,
    ];
}

/// Formatting modes for the native Numbers title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleNumbersFormat {
    Number,
    Timecode,
    Date,
    Currency,
    Percent,
}

impl TitleNumbersFormat {
    pub const fn uses_decimal_controls(self) -> bool {
        matches!(self, Self::Number | Self::Currency | Self::Percent)
    }

    pub(super) fn read_from(properties: &PropertyBag) -> Result<Self> {
        match required_enum(
            properties,
            BasicTitle::NUMBERS_FORMAT_PATH,
            TimelineTime::ZERO,
        )?
        .as_str()
        {
            "number" => Ok(Self::Number),
            "timecode" => Ok(Self::Timecode),
            "date" => Ok(Self::Date),
            "currency" => Ok(Self::Currency),
            "percent" => Ok(Self::Percent),
            _ => Err(title_error("unknown Numbers Title format")),
        }
    }
}

pub(super) fn descriptors() -> [PropertyDescriptor; 8] {
    let field = |path, id, name, value| {
        PropertyDescriptor::new(path, name, value)
            .with_parameter_id(ParameterId::new_static(id))
            .with_animatable(false)
    };
    let numeric = |min, max, step| {
        ParameterNumericContract::closed(min, max, Some(step), ParameterInvalidValuePolicy::Reject)
            .expect("valid Numbers Title numeric contract")
    };
    let control = || {
        ParameterNumericContract::new(
            ParameterNumericRange { min: -MAX_CONTROL, max: MAX_CONTROL },
            ParameterNumericRange { min: -1000.0, max: 1000.0 },
            Some(0.000_000_01),
            ParameterInvalidValuePolicy::Reject,
        )
        .expect("valid Numbers Title control range")
    };
    [
        field(
            BasicTitle::NUMBERS_FORMAT_PATH,
            "mondrian.title.numbers.format",
            "类型",
            PropertyValue::Enum("number".to_owned()),
        )
        .with_enum_options(enum_options(&[
            ("number", "Number"),
            ("timecode", "Timecode"),
            ("date", "Date"),
            ("currency", "Currency"),
            ("percent", "Percent"),
        ])),
        field(
            BasicTitle::NUMBERS_START_PATH,
            "mondrian.title.numbers.start",
            "起始值",
            PropertyValue::Double(0.0),
        )
        .with_numeric_contract(ParameterUnit::Unitless, control()),
        field(
            BasicTitle::NUMBERS_STEP_PATH,
            "mondrian.title.numbers.step",
            "步长",
            PropertyValue::Double(1.0),
        )
        .with_numeric_contract(ParameterUnit::Unitless, control()),
        field(
            BasicTitle::NUMBERS_DECIMALS_PATH,
            "mondrian.title.numbers.decimals",
            "小数位",
            PropertyValue::Int(0),
        )
        .with_numeric_contract(ParameterUnit::Unitless, numeric(0.0, 8.0, 1.0)),
        field(
            BasicTitle::NUMBERS_GROUP_PATH,
            "mondrian.title.numbers.group_thousands",
            "千分位",
            PropertyValue::Bool(false),
        ),
        field(
            BasicTitle::NUMBERS_PREFIX_PATH,
            "mondrian.title.numbers.prefix",
            "前缀",
            PropertyValue::Text(String::new()),
        )
        .with_text_mode(ParameterTextMode::SingleLine),
        field(
            BasicTitle::NUMBERS_SUFFIX_PATH,
            "mondrian.title.numbers.suffix",
            "后缀",
            PropertyValue::Text(String::new()),
        )
        .with_text_mode(ParameterTextMode::SingleLine),
        field(
            BasicTitle::NUMBERS_ZERO_PAD_PATH,
            "mondrian.title.numbers.zero_pad",
            "补零位数",
            PropertyValue::Int(0),
        )
        .with_numeric_contract(ParameterUnit::Unitless, numeric(0.0, 12.0, 1.0)),
    ]
}

pub(super) fn validate(properties: &PropertyBag) -> Result<()> {
    let (prefix, suffix) = affixes(properties)?;
    if prefix.len() + suffix.len() > BASIC_TITLE_MAX_TEXT_BYTES - LABEL_BYTES {
        return Err(title_error(
            "Numbers Title affixes exceed the title text budget",
        ));
    }
    Ok(())
}

fn affixes(properties: &PropertyBag) -> Result<(String, String)> {
    Ok((
        required_text(
            properties,
            BasicTitle::NUMBERS_PREFIX_PATH,
            TimelineTime::ZERO,
        )?,
        required_text(
            properties,
            BasicTitle::NUMBERS_SUFFIX_PATH,
            TimelineTime::ZERO,
        )?,
    ))
}

pub(super) fn label(
    properties: &PropertyBag,
    clip_time: TimelineTime,
    sequence_rate: Rational,
    sequence_display: TimelineDisplaySettings,
) -> Result<String> {
    let frame = clip_time.to_frame_position(sequence_rate, FrameRounding::Floor)?.frame;
    let control = |path| match properties.evaluate(path, TimelineTime::ZERO) {
        Some(PropertyValue::Double(value)) => fixed_control(value),
        _ => Err(title_error(format!(
            "Numbers Title property `{path}` must be numeric"
        ))),
    };
    let start = control(BasicTitle::NUMBERS_START_PATH)?;
    let step = control(BasicTitle::NUMBERS_STEP_PATH)?;
    // |start| and |step| are at most 10^17. Even an i64 frame fits in i128 here.
    let value = start + step * i128::from(frame);
    let format = TitleNumbersFormat::read_from(properties)?;
    let rendered = match format {
        TitleNumbersFormat::Timecode => {
            let counting = match sequence_display.format {
                TimelineDisplayFormat::Timecode(mode) => mode,
                TimelineDisplayFormat::Frames => SmpteCountingMode::NonDropFrame,
            };
            let actual_frame = i64::try_from(value.div_euclid(DECIMAL_SCALE)).map_err(|_| {
                title_error("Numbers Title timecode exceeds the signed frame range")
            })?;
            SmpteDisplayTimecodeContract::new(sequence_rate, counting, 0)
                .and_then(|contract| contract.timecode_at_frame(actual_frame))
                .map_err(|error| title_error(error.to_string()))?
                .label()
        }
        TitleNumbersFormat::Date => {
            let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid Unix epoch");
            let date = i64::try_from(value.div_euclid(DECIMAL_SCALE))
                .ok()
                .and_then(chrono::TimeDelta::try_days)
                .and_then(|offset| epoch.checked_add_signed(offset))
                .ok_or_else(|| title_error("Numbers Title date exceeds the supported calendar"))?;
            date.format("%Y-%m-%d").to_string()
        }
        TitleNumbersFormat::Number | TitleNumbersFormat::Currency | TitleNumbersFormat::Percent => {
            let decimals = required_int(
                properties,
                BasicTitle::NUMBERS_DECIMALS_PATH,
                TimelineTime::ZERO,
            )?;
            let zero_pad = required_int(
                properties,
                BasicTitle::NUMBERS_ZERO_PAD_PATH,
                TimelineTime::ZERO,
            )?;
            if !(0..=8).contains(&decimals) || !(0..=12).contains(&zero_pad) {
                return Err(title_error("Numbers Title digit settings are out of range"));
            }
            let group = group_thousands(properties)?;
            format_decimal(value, decimals as u32, zero_pad as usize, group, format)
        }
    };
    let (prefix, suffix) = affixes(properties)?;
    Ok(format!("{prefix}{rendered}{suffix}"))
}

fn group_thousands(properties: &PropertyBag) -> Result<bool> {
    match properties.evaluate(BasicTitle::NUMBERS_GROUP_PATH, TimelineTime::ZERO) {
        Some(PropertyValue::Bool(value)) => Ok(value),
        _ => Err(title_error("Numbers Title grouping must be boolean")),
    }
}

fn fixed_control(value: f64) -> Result<i128> {
    if !value.is_finite() || value.abs() > MAX_CONTROL {
        return Err(title_error("Numbers Title numeric control is out of range"));
    }
    // Interpret the authored decimal representation before accumulating frames.
    let decimal = value.abs().to_string();
    let (whole, fraction) = decimal.split_once('.').unwrap_or((decimal.as_str(), ""));
    let whole: i128 = whole.parse().map_err(|_| title_error("invalid Numbers Title decimal"))?;
    let mut units = 0i128;
    for byte in fraction.bytes().take(8) {
        units = units * 10 + i128::from(byte - b'0');
    }
    units *= 10i128.pow(8 - fraction.len().min(8) as u32);
    if fraction.as_bytes().get(8).is_some_and(|byte| *byte >= b'5') {
        units += 1;
    }
    let scaled = whole * DECIMAL_SCALE + units;
    Ok(if value.is_sign_negative() {
        -scaled
    } else {
        scaled
    })
}

fn format_decimal(
    value: i128,
    decimals: u32,
    zero_pad: usize,
    group: bool,
    format: TitleNumbersFormat,
) -> String {
    let divisor = 10i128.pow(8 - decimals);
    let magnitude = (value.abs() + divisor / 2) / divisor;
    let digits = 10i128.pow(decimals);
    let integral = format!("{:0zero_pad$}", magnitude / digits);
    let mut label = String::new();
    if value < 0 && magnitude != 0 {
        label.push('-');
    }
    if format == TitleNumbersFormat::Currency {
        label.push('$');
    }
    for (index, digit) in integral.bytes().enumerate() {
        if group && index > 0 && (integral.len() - index).is_multiple_of(3) {
            label.push(',');
        }
        label.push(char::from(digit));
    }
    if decimals != 0 {
        use std::fmt::Write as _;
        write!(
            label,
            ".{:0width$}",
            magnitude % digits,
            width = decimals as usize
        )
        .expect("writing to String is infallible");
    }
    if format == TitleNumbersFormat::Percent {
        label.push('%');
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PropertyMutation;

    fn render(
        fields: &[(&str, PropertyValue)],
        clip_time: TimelineTime,
        rate: Rational,
        display: TimelineDisplaySettings,
    ) -> String {
        let mut title = BasicTitle::new_numbers("Arial").unwrap();
        for (path, value) in fields {
            title
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: (*path).to_owned(),
                    value: value.clone(),
                })
                .unwrap();
        }
        title
            .evaluate_for_render(clip_time, TimelineTime::ZERO, rate, display, None)
            .unwrap()
            .text
    }

    #[test]
    fn numbers_title_accumulates_decimal_steps_on_the_exact_clip_grid() {
        let fields = [
            (BasicTitle::NUMBERS_START_PATH, PropertyValue::Double(0.1)),
            (BasicTitle::NUMBERS_STEP_PATH, PropertyValue::Double(-0.1)),
            (BasicTitle::NUMBERS_DECIMALS_PATH, PropertyValue::Int(8)),
        ];
        let rate = Rational::FPS_24;
        let display = TimelineDisplaySettings::frames(700);
        assert_eq!(
            render(&fields, TimelineTime::new(3, 24).unwrap(), rate, display),
            "-0.20000000"
        );
        assert_eq!(
            render(&fields, TimelineTime::new(-1, 48).unwrap(), rate, display),
            "0.20000000"
        );
    }

    #[test]
    fn numbers_title_rounds_then_pads_and_groups_without_negative_zero() {
        let mut fields = [
            (
                BasicTitle::NUMBERS_START_PATH,
                PropertyValue::Double(-999.995),
            ),
            (BasicTitle::NUMBERS_STEP_PATH, PropertyValue::Double(0.0)),
            (BasicTitle::NUMBERS_DECIMALS_PATH, PropertyValue::Int(2)),
            (BasicTitle::NUMBERS_ZERO_PAD_PATH, PropertyValue::Int(7)),
            (BasicTitle::NUMBERS_GROUP_PATH, PropertyValue::Bool(true)),
            (
                BasicTitle::NUMBERS_FORMAT_PATH,
                PropertyValue::Enum("currency".to_owned()),
            ),
            (
                BasicTitle::NUMBERS_PREFIX_PATH,
                PropertyValue::Text("[".to_owned()),
            ),
            (
                BasicTitle::NUMBERS_SUFFIX_PATH,
                PropertyValue::Text("]".to_owned()),
            ),
        ];
        let output = |fields: &[(&str, PropertyValue)]| {
            render(
                fields,
                TimelineTime::ZERO,
                Rational::FPS_24,
                TimelineDisplaySettings::frames(0),
            )
        };
        assert_eq!(output(&fields), "[-$0,001,000.00]");
        fields[0].1 = PropertyValue::Double(-0.004_999);
        fields[5].1 = PropertyValue::Enum("percent".to_owned());
        assert_eq!(output(&fields), "[0,000,000.00%]");
    }

    #[test]
    fn numbers_title_uses_the_epoch_and_actual_drop_frame_counts() {
        let mut fields = [
            (
                BasicTitle::NUMBERS_FORMAT_PATH,
                PropertyValue::Enum("date".to_owned()),
            ),
            (
                BasicTitle::NUMBERS_START_PATH,
                PropertyValue::Double(18_321.0),
            ),
        ];
        let rate = Rational::FPS_2997;
        let display = TimelineDisplaySettings::timecode(SmpteCountingMode::DropFrame, 100);
        assert_eq!(
            render(&fields, TimelineTime::ZERO, rate, display),
            "2020-02-29"
        );
        fields[1].1 = PropertyValue::Double(-0.25);
        assert_eq!(
            render(&fields, TimelineTime::ZERO, rate, display),
            "1969-12-31"
        );
        fields[0].1 = PropertyValue::Enum("timecode".to_owned());
        fields[1].1 = PropertyValue::Double(1800.0);
        assert_eq!(
            render(&fields, TimelineTime::ZERO, rate, display),
            "00:01:00;02"
        );
    }
}
