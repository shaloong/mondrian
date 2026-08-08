//! Authoring contract for generated timeline titles.
//!
//! A title is sequence-local Clip content. It is not a media Asset and it is
//! not an Effect: the title generates straight-alpha picture which then enters
//! the ordinary Clip transform, Effect, Mask, blend, Preview, and Export path.

use crate::automation::{
    ParameterEnumOption, ParameterInvalidValuePolicy, ParameterNumericContract, ParameterUnit,
    PropertyBag, PropertyDescriptor, PropertyMutation, PropertyValue,
};
use crate::{Color, MondrianError, ParameterId, Result, TimelineTime};
use serde::{Deserialize, Serialize};

/// Maximum persisted UTF-8 bytes admitted for one Basic Title.
pub const BASIC_TITLE_MAX_TEXT_BYTES: usize = 64 * 1024;
/// Maximum persisted UTF-8 bytes admitted for one requested font family.
pub const BASIC_TITLE_MAX_FONT_FAMILY_BYTES: usize = 512;

/// Horizontal alignment of a Basic Title inside the Sequence canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasicTitleHorizontalAlign {
    /// Align the shaped lines to the left.
    Left,
    /// Center the shaped lines.
    Center,
    /// Align the shaped lines to the right.
    Right,
}

/// Vertical alignment of a Basic Title inside the Sequence canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasicTitleVerticalAlign {
    /// Align the shaped block to the top title-safe boundary.
    Top,
    /// Center the shaped block vertically.
    Center,
    /// Align the shaped block to the bottom title-safe boundary.
    Bottom,
}

/// Requested font posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasicTitleFontStyle {
    /// Upright face.
    Normal,
    /// True italic face.
    Italic,
    /// Mechanically or natively oblique face.
    Oblique,
}

/// Fully evaluated Basic Title semantics at one title-local author time.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedBasicTitle {
    /// UTF-8 text, including explicit line breaks.
    pub text: String,
    /// Exact requested system font family. Missing families fail closed.
    pub font_family: String,
    /// OpenType/CSS numeric weight in the inclusive 1–1000 range.
    pub font_weight: u16,
    /// Requested font posture.
    pub font_style: BasicTitleFontStyle,
    /// Font size in full-resolution Sequence pixels.
    pub font_size: f32,
    /// Straight-alpha fill color in the Sequence working-linear RGB space.
    pub fill: Color,
    /// Additional glyph tracking in em units.
    pub tracking_em: f32,
    /// Line height as a multiplier of font size.
    pub line_height: f32,
    /// Horizontal canvas alignment.
    pub horizontal_align: BasicTitleHorizontalAlign,
    /// Vertical canvas alignment.
    pub vertical_align: BasicTitleVerticalAlign,
}

/// Sequence-local Basic Title author state.
///
/// The Property Bag is a closed, definition-backed set. Unknown, missing, or
/// schema-divergent properties are rejected before a Project snapshot can
/// enter an Authoring Session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BasicTitle {
    properties: PropertyBag,
}

impl BasicTitle {
    /// Current property address for title text.
    pub const TEXT_PATH: &'static str = "title.text";
    /// Current property address for the requested font family.
    pub const FONT_FAMILY_PATH: &'static str = "title.font_family";
    /// Current property address for numeric font weight.
    pub const FONT_WEIGHT_PATH: &'static str = "title.font_weight";
    /// Current property address for font posture.
    pub const FONT_STYLE_PATH: &'static str = "title.font_style";
    /// Current property address for font size.
    pub const FONT_SIZE_PATH: &'static str = "title.font_size";
    /// Current property address for working-linear fill.
    pub const FILL_PATH: &'static str = "title.fill";
    /// Current property address for glyph tracking.
    pub const TRACKING_PATH: &'static str = "title.tracking";
    /// Current property address for line-height multiplier.
    pub const LINE_HEIGHT_PATH: &'static str = "title.line_height";
    /// Current property address for horizontal alignment.
    pub const HORIZONTAL_ALIGN_PATH: &'static str = "title.horizontal_align";
    /// Current property address for vertical alignment.
    pub const VERTICAL_ALIGN_PATH: &'static str = "title.vertical_align";
    /// Canonical Basic Title property order used by authoring Adapters.
    pub const PROPERTY_PATHS: [&'static str; 10] = [
        Self::TEXT_PATH,
        Self::FONT_FAMILY_PATH,
        Self::FONT_WEIGHT_PATH,
        Self::FONT_STYLE_PATH,
        Self::FONT_SIZE_PATH,
        Self::FILL_PATH,
        Self::TRACKING_PATH,
        Self::LINE_HEIGHT_PATH,
        Self::HORIZONTAL_ALIGN_PATH,
        Self::VERTICAL_ALIGN_PATH,
    ];

    /// Build a validated Basic Title with a concrete named font dependency.
    pub fn new(text: impl Into<String>, font_family: impl Into<String>) -> Result<Self> {
        let text = text.into();
        let font_family = font_family.into();
        let mut properties = PropertyBag::default();
        for descriptor in canonical_descriptors() {
            properties.define(descriptor);
        }
        properties.set_static_value(Self::TEXT_PATH, PropertyValue::Text(text))?;
        properties.set_static_value(Self::FONT_FAMILY_PATH, PropertyValue::Text(font_family))?;
        let title = Self { properties };
        title.validate_author_state()?;
        Ok(title)
    }

    /// Return the closed property set for Inspector and command routing.
    pub fn property_bag(&self) -> PropertyBag {
        self.properties.clone()
    }

    /// Apply one validated author mutation to the title property set.
    pub fn apply_property_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        if matches!(mutation, PropertyMutation::RemoveProperty { .. }) {
            return Err(title_error(
                "built-in Basic Title properties cannot be removed",
            ));
        }
        self.properties.apply_mutation(mutation)?;
        self.validate_author_state()
    }

    /// Fork placement-local animation and keyframe identities.
    pub fn fork_author_identities(&mut self) {
        self.properties.fork_author_identities();
    }

    /// Validate the complete persisted author contract.
    pub fn validate_author_state(&self) -> Result<()> {
        self.properties.validate()?;
        let canonical = canonical_descriptors();
        if self.properties.iter().count() != canonical.len() {
            return Err(title_error(
                "Basic Title property set must contain exactly the canonical definitions",
            ));
        }
        for expected in canonical {
            let actual = self.properties.property(&expected.path).ok_or_else(|| {
                title_error(format!(
                    "Basic Title property `{}` is missing",
                    expected.path
                ))
            })?;
            if actual.descriptor.schema != expected.schema
                || actual.descriptor.ui_metadata != expected.ui_metadata
            {
                return Err(title_error(format!(
                    "Basic Title property `{}` does not match its canonical schema",
                    expected.path
                )));
            }
        }

        let evaluated = self.evaluate(TimelineTime::ZERO)?;
        if evaluated.text.len() > BASIC_TITLE_MAX_TEXT_BYTES {
            return Err(title_error(format!(
                "Basic Title text exceeds {BASIC_TITLE_MAX_TEXT_BYTES} UTF-8 bytes"
            )));
        }
        if evaluated.font_family.trim().is_empty() {
            return Err(title_error("Basic Title font family cannot be empty"));
        }
        if evaluated.font_family.len() > BASIC_TITLE_MAX_FONT_FAMILY_BYTES {
            return Err(title_error(format!(
                "Basic Title font family exceeds {BASIC_TITLE_MAX_FONT_FAMILY_BYTES} UTF-8 bytes"
            )));
        }
        Ok(())
    }

    /// Evaluate every title parameter at one title-local author time.
    pub fn evaluate(&self, time: TimelineTime) -> Result<EvaluatedBasicTitle> {
        let text = required_text(&self.properties, Self::TEXT_PATH, time)?;
        let font_family = required_text(&self.properties, Self::FONT_FAMILY_PATH, time)?;
        let font_weight = required_int(&self.properties, Self::FONT_WEIGHT_PATH, time)?;
        let font_weight = u16::try_from(font_weight)
            .map_err(|_| title_error("Basic Title font weight is outside u16 range"))?;
        let font_style =
            match required_enum(&self.properties, Self::FONT_STYLE_PATH, time)?.as_str() {
                "normal" => BasicTitleFontStyle::Normal,
                "italic" => BasicTitleFontStyle::Italic,
                "oblique" => BasicTitleFontStyle::Oblique,
                _ => return Err(title_error("Basic Title font style is not canonical")),
            };
        let horizontal_align =
            match required_enum(&self.properties, Self::HORIZONTAL_ALIGN_PATH, time)?.as_str() {
                "left" => BasicTitleHorizontalAlign::Left,
                "center" => BasicTitleHorizontalAlign::Center,
                "right" => BasicTitleHorizontalAlign::Right,
                _ => {
                    return Err(title_error(
                        "Basic Title horizontal alignment is not canonical",
                    ))
                }
            };
        let vertical_align =
            match required_enum(&self.properties, Self::VERTICAL_ALIGN_PATH, time)?.as_str() {
                "top" => BasicTitleVerticalAlign::Top,
                "center" => BasicTitleVerticalAlign::Center,
                "bottom" => BasicTitleVerticalAlign::Bottom,
                _ => {
                    return Err(title_error(
                        "Basic Title vertical alignment is not canonical",
                    ))
                }
            };

        Ok(EvaluatedBasicTitle {
            text,
            font_family,
            font_weight,
            font_style,
            font_size: required_float(&self.properties, Self::FONT_SIZE_PATH, time)?,
            fill: required_color(&self.properties, Self::FILL_PATH, time)?,
            tracking_em: required_float(&self.properties, Self::TRACKING_PATH, time)?,
            line_height: required_float(&self.properties, Self::LINE_HEIGHT_PATH, time)?,
            horizontal_align,
            vertical_align,
        })
    }
}

fn canonical_descriptors() -> Vec<PropertyDescriptor> {
    vec![
        PropertyDescriptor::new(
            BasicTitle::TEXT_PATH,
            "文字",
            PropertyValue::Text("Title".to_owned()),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.text"))
        .with_animatable(false),
        PropertyDescriptor::new(
            BasicTitle::FONT_FAMILY_PATH,
            "字体",
            PropertyValue::Text(default_basic_title_font_family().to_owned()),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.font_family"))
        .with_animatable(false),
        PropertyDescriptor::new(
            BasicTitle::FONT_WEIGHT_PATH,
            "字重",
            PropertyValue::Int(400),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.font_weight"))
        .with_numeric_contract(
            ParameterUnit::Unitless,
            ParameterNumericContract::closed(
                1.0,
                1000.0,
                Some(1.0),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("Basic Title weight constants form a valid numeric contract"),
        )
        .with_animatable(false),
        PropertyDescriptor::new(
            BasicTitle::FONT_STYLE_PATH,
            "字形",
            PropertyValue::Enum("normal".to_owned()),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.font_style"))
        .with_enum_options(enum_options(&["normal", "italic", "oblique"]))
        .with_animatable(false),
        PropertyDescriptor::new(
            BasicTitle::FONT_SIZE_PATH,
            "字号",
            PropertyValue::Float(96.0),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.font_size"))
        .with_numeric_contract(
            ParameterUnit::Pixels,
            ParameterNumericContract::closed(
                1.0,
                4096.0,
                Some(1.0),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("Basic Title size constants form a valid numeric contract"),
        ),
        PropertyDescriptor::new(
            BasicTitle::FILL_PATH,
            "填充",
            PropertyValue::Color(Color::WHITE),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.fill")),
        PropertyDescriptor::new(BasicTitle::TRACKING_PATH, "字距", PropertyValue::Float(0.0))
            .with_parameter_id(ParameterId::new_static("mondrian.title.tracking"))
            .with_numeric_contract(
                ParameterUnit::Normalized,
                ParameterNumericContract::closed(
                    -1.0,
                    10.0,
                    Some(0.01),
                    ParameterInvalidValuePolicy::Reject,
                )
                .expect("Basic Title tracking constants form a valid numeric contract"),
            ),
        PropertyDescriptor::new(
            BasicTitle::LINE_HEIGHT_PATH,
            "行高",
            PropertyValue::Float(1.2),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.line_height"))
        .with_numeric_contract(
            ParameterUnit::Normalized,
            ParameterNumericContract::closed(
                0.5,
                10.0,
                Some(0.05),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("Basic Title line-height constants form a valid numeric contract"),
        ),
        PropertyDescriptor::new(
            BasicTitle::HORIZONTAL_ALIGN_PATH,
            "水平对齐",
            PropertyValue::Enum("center".to_owned()),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.horizontal_align"))
        .with_enum_options(enum_options(&["left", "center", "right"]))
        .with_animatable(false),
        PropertyDescriptor::new(
            BasicTitle::VERTICAL_ALIGN_PATH,
            "垂直对齐",
            PropertyValue::Enum("center".to_owned()),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.vertical_align"))
        .with_enum_options(enum_options(&["top", "center", "bottom"]))
        .with_animatable(false),
    ]
}

fn enum_options(keys: &[&str]) -> Vec<ParameterEnumOption> {
    keys.iter()
        .map(|key| ParameterEnumOption::new(*key, format!("mondrian.title.option.{key}")))
        .collect()
}

/// Default concrete font dependency for the Windows Alpha product.
pub const fn default_basic_title_font_family() -> &'static str {
    if cfg!(target_os = "windows") {
        "Microsoft YaHei"
    } else if cfg!(target_os = "macos") {
        "Helvetica"
    } else {
        "DejaVu Sans"
    }
}

fn required_text(properties: &PropertyBag, path: &str, time: TimelineTime) -> Result<String> {
    match properties.evaluate(path, time) {
        Some(PropertyValue::Text(value)) => Ok(value),
        _ => Err(title_error(format!(
            "Basic Title property `{path}` must be text"
        ))),
    }
}

fn required_int(properties: &PropertyBag, path: &str, time: TimelineTime) -> Result<i64> {
    match properties.evaluate(path, time) {
        Some(PropertyValue::Int(value)) => Ok(value),
        _ => Err(title_error(format!(
            "Basic Title property `{path}` must be an integer"
        ))),
    }
}

fn required_float(properties: &PropertyBag, path: &str, time: TimelineTime) -> Result<f32> {
    match properties.evaluate(path, time) {
        Some(PropertyValue::Float(value)) => Ok(value),
        _ => Err(title_error(format!(
            "Basic Title property `{path}` must be a float"
        ))),
    }
}

fn required_color(properties: &PropertyBag, path: &str, time: TimelineTime) -> Result<Color> {
    match properties.evaluate(path, time) {
        Some(PropertyValue::Color(value)) => Ok(value),
        _ => Err(title_error(format!(
            "Basic Title property `{path}` must be a color"
        ))),
    }
}

fn required_enum(properties: &PropertyBag, path: &str, time: TimelineTime) -> Result<String> {
    match properties.evaluate(path, time) {
        Some(PropertyValue::Enum(value)) => Ok(value),
        _ => Err(title_error(format!(
            "Basic Title property `{path}` must be an enum"
        ))),
    }
}

fn title_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "basic_title_author_state".to_owned(),
        reason: reason.into(),
    }
}

impl crate::AuthoringFootprint for BasicTitle {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> std::result::Result<(), crate::AuthoringFootprintError> {
        let Self { properties } = self;
        collector.collect(properties)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::Keyframe;

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::new(frame, 24).expect("valid test time")
    }

    #[test]
    fn title_has_one_closed_canonical_property_set() {
        let title = BasicTitle::new("Mondrian 标题", default_basic_title_font_family())
            .expect("valid title");
        title.validate_author_state().expect("canonical title");
        let evaluated = title.evaluate(TimelineTime::ZERO).expect("evaluate");

        assert_eq!(evaluated.text, "Mondrian 标题");
        assert_eq!(evaluated.font_family, default_basic_title_font_family());
        assert_eq!(evaluated.font_weight, 400);
        assert_eq!(evaluated.font_size, 96.0);
        assert_eq!(
            evaluated.horizontal_align,
            BasicTitleHorizontalAlign::Center
        );
        assert_eq!(title.property_bag().iter().count(), 10);
    }

    #[test]
    fn numeric_title_parameters_use_exact_author_time_animation() {
        let mut title =
            BasicTitle::new("Animated", default_basic_title_font_family()).expect("title");
        title
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: BasicTitle::FONT_SIZE_PATH.to_owned(),
                keyframe: Keyframe::linear(tt(0), PropertyValue::Float(40.0)),
            })
            .expect("first key");
        title
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: BasicTitle::FONT_SIZE_PATH.to_owned(),
                keyframe: Keyframe::linear(tt(24), PropertyValue::Float(80.0)),
            })
            .expect("second key");

        assert!((title.evaluate(tt(12)).expect("midpoint").font_size - 60.0).abs() < 1.0e-5);
    }

    #[test]
    fn title_rejects_removed_or_schema_divergent_properties() {
        let title = BasicTitle::new("Title", default_basic_title_font_family()).expect("title");
        let mut value = serde_json::to_value(title).expect("serialize");
        let properties = value["properties"]["properties"].as_object_mut().expect("property map");
        properties.remove(BasicTitle::FONT_SIZE_PATH);
        let title: BasicTitle = serde_json::from_value(value).expect("deserialize candidate");

        assert!(title.validate_author_state().is_err());
    }

    #[test]
    fn title_fork_changes_every_property_owner_identity() {
        let mut title = BasicTitle::new("Fork", default_basic_title_font_family()).expect("title");
        let before: Vec<_> =
            title.property_bag().iter().map(|(_, property)| property.track_id).collect();
        title.fork_author_identities();
        let after: Vec<_> =
            title.property_bag().iter().map(|(_, property)| property.track_id).collect();

        assert_eq!(before.len(), after.len());
        assert!(before.iter().zip(after).all(|(before, after)| *before != after));
    }
}
