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

mod clip_info;
mod metadata_burn_in;
mod numbers;
mod timecode;
pub use clip_info::{TitleAssetMetadata, TitleAssetNames, TitleClipInfo, TitleClipInfoField};
pub use metadata_burn_in::{
    TitleMetadataField, TitleMetadataLine, TitleMetadataLinePaths, TITLE_METADATA_LINE_PATHS,
};
pub use numbers::TitleNumbersFormat;
pub use timecode::{
    TitleMediaTimecodeSample, TitleTimecode, TitleTimecodeFormat, TitleTimecodeSource,
};

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

/// Static decoration controls in full-resolution Sequence pixels and working-linear color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BasicTitleAppearance {
    /// Extent of the outline under the fill.
    pub stroke_width: f32,
    /// Straight-alpha outline color.
    pub stroke_color: Color,
    /// Shadow offset along the down-right diagonal.
    pub shadow_distance: f32,
    /// Radius of the finite-support shadow blur.
    pub shadow_softness: f32,
    /// Straight-alpha shadow color.
    pub shadow_color: Color,
    /// Straight-alpha background color.
    pub background_color: Color,
    /// Additional opacity of the background, in [0, 1].
    pub background_opacity: f32,
    /// Overall generated title opacity, in [0, 1], before Clip opacity.
    pub opacity: f32,
}

impl Default for BasicTitleAppearance {
    fn default() -> Self {
        Self {
            stroke_width: 0.0,
            stroke_color: Color::BLACK,
            shadow_distance: 0.0,
            shadow_softness: 10.0,
            shadow_color: Color::BLACK,
            background_color: Color::BLACK,
            background_opacity: 0.0,
            opacity: 1.0,
        }
    }
}

/// Fully evaluated Basic Title semantics at one title-local author time.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedBasicTitle {
    /// Explicit author-pixel layout anchor for generated captions.
    pub layout_anchor: Option<[f32; 2]>,
    /// Metadata lines with independent author-pixel anchors and sizes.
    pub positioned_lines: Vec<PositionedTitleLine>,
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
    /// Static title decorations, independent of the Clip transform and opacity.
    pub appearance: BasicTitleAppearance,
}

/// One resolved line using the common style of its owning title.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionedTitleLine {
    pub text: String,
    pub anchor: [f32; 2],
    pub font_size: f32,
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
    #[serde(default, skip_serializing_if = "BasicTitleContentKind::is_text")]
    content_kind: BasicTitleContentKind,
}

/// The authoring content generated by a native title Clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasicTitleContentKind {
    #[default]
    Text,
    Timecode,
    Numbers,
    ClipInfo,
    MetadataBurnIn,
}

impl BasicTitleContentKind {
    fn is_text(&self) -> bool {
        *self == Self::Text
    }
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
    /// Current property address for outline extent.
    pub const STROKE_WIDTH_PATH: &'static str = "title.stroke_width";
    /// Current property address for working-linear outline color.
    pub const STROKE_COLOR_PATH: &'static str = "title.stroke_color";
    /// Current property address for shadow offset.
    pub const SHADOW_DISTANCE_PATH: &'static str = "title.shadow_distance";
    /// Current property address for shadow blur radius.
    pub const SHADOW_SOFTNESS_PATH: &'static str = "title.shadow_softness";
    /// Current property address for working-linear shadow color.
    pub const SHADOW_COLOR_PATH: &'static str = "title.shadow_color";
    /// Current property address for working-linear backdrop color.
    pub const BACKGROUND_COLOR_PATH: &'static str = "title.background_color";
    /// Current property address for backdrop opacity.
    pub const BACKGROUND_OPACITY_PATH: &'static str = "title.background_opacity";
    /// Current property address for generated title opacity.
    pub const OPACITY_PATH: &'static str = "title.opacity";
    /// Canonical Basic Title property order used by authoring Adapters.
    pub const PROPERTY_PATHS: [&'static str; 18] = [
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
        Self::STROKE_WIDTH_PATH,
        Self::STROKE_COLOR_PATH,
        Self::SHADOW_DISTANCE_PATH,
        Self::SHADOW_SOFTNESS_PATH,
        Self::SHADOW_COLOR_PATH,
        Self::BACKGROUND_COLOR_PATH,
        Self::BACKGROUND_OPACITY_PATH,
        Self::OPACITY_PATH,
    ];

    /// Build a validated Basic Title with a concrete named font dependency.
    pub fn new(text: impl Into<String>, font_family: impl Into<String>) -> Result<Self> {
        let title =
            Self::with_properties(text.into(), font_family.into(), BasicTitleContentKind::Text)?;
        title.validate_author_state()?;
        Ok(title)
    }

    fn with_properties(
        text: String,
        font_family: String,
        content_kind: BasicTitleContentKind,
    ) -> Result<Self> {
        let mut properties = PropertyBag::default();
        for descriptor in canonical_descriptors(content_kind) {
            properties.define(descriptor);
        }
        properties.set_static_value(Self::TEXT_PATH, PropertyValue::Text(text))?;
        properties.set_static_value(Self::FONT_FAMILY_PATH, PropertyValue::Text(font_family))?;
        Ok(Self { properties, content_kind })
    }

    /// Create a native overlay with authored clock settings and independent styling.
    pub fn new_timecode(timecode: TitleTimecode, font_family: impl Into<String>) -> Result<Self> {
        let mut title = Self::with_properties(
            String::new(),
            font_family.into(),
            BasicTitleContentKind::Timecode,
        )?;
        timecode.write_to(&mut title.properties)?;
        title.validate_author_state()?;
        Ok(title)
    }

    /// Create a frame-based counter title using its canonical author defaults.
    pub fn new_numbers(font_family: impl Into<String>) -> Result<Self> {
        let title = Self::with_properties(
            String::new(),
            font_family.into(),
            BasicTitleContentKind::Numbers,
        )?;
        title.validate_author_state()?;
        Ok(title)
    }

    pub const fn content_kind(&self) -> BasicTitleContentKind {
        self.content_kind
    }

    /// Create a native overlay for editorial Clip metadata.
    pub fn new_clip_info(font_family: impl Into<String>) -> Result<Self> {
        let title = Self::with_properties(
            String::new(),
            font_family.into(),
            BasicTitleContentKind::ClipInfo,
        )?;
        title.validate_author_state()?;
        Ok(title)
    }

    pub fn clip_info(&self) -> Result<Option<TitleClipInfo>> {
        matches!(self.content_kind, BasicTitleContentKind::ClipInfo)
            .then(|| TitleClipInfo::read_from(&self.properties))
            .transpose()
    }

    /// Create a five-line overlay with a visible sequence timecode on the first line.
    pub fn new_metadata_burn_in(font_family: impl Into<String>) -> Result<Self> {
        let title = Self::with_properties(
            String::new(),
            font_family.into(),
            BasicTitleContentKind::MetadataBurnIn,
        )?;
        title.validate_author_state()?;
        Ok(title)
    }

    /// Read each canonical line in stacking order.
    pub fn metadata_lines(&self) -> Result<Option<[TitleMetadataLine; 5]>> {
        if self.content_kind != BasicTitleContentKind::MetadataBurnIn {
            return Ok(None);
        }
        let [first, second, third, fourth, fifth] = TITLE_METADATA_LINE_PATHS
            .map(|paths| TitleMetadataLine::read_from(&self.properties, paths));
        Ok(Some([first?, second?, third?, fourth?, fifth?]))
    }

    /// Retarget line sources when their owner Sequence is copied.
    pub fn remap_metadata_tracks(
        &mut self,
        mut resolve: impl FnMut(crate::TrackId) -> Option<crate::TrackId>,
    ) -> Result<()> {
        if let Some(lines) = self.metadata_lines()? {
            for (line, paths) in lines.into_iter().zip(TITLE_METADATA_LINE_PATHS) {
                if let Some(id) = line.track_id.and_then(&mut resolve) {
                    self.properties
                        .set_static_value(paths.track, PropertyValue::Text(id.to_string()))?;
                }
            }
        }
        Ok(())
    }

    /// Retarget the source when an owning Sequence is duplicated with its tracks.
    pub fn remap_clip_info_track(
        &mut self,
        resolve: impl FnOnce(crate::TrackId) -> Option<crate::TrackId>,
    ) -> Result<()> {
        if let Some(id) = self.clip_info()?.and_then(|info| info.track_id).and_then(resolve) {
            self.properties.set_static_value(
                Self::CLIP_INFO_TRACK_PATH,
                PropertyValue::Text(id.to_string()),
            )?;
        }
        Ok(())
    }

    /// Place a sampled metadata label at its normalized Sequence anchor.
    pub fn evaluate_clip_info_for_render(
        &self,
        clip_time: TimelineTime,
        info: TitleClipInfo,
        label: &str,
        resolution: crate::Resolution,
    ) -> Result<EvaluatedBasicTitle> {
        self.evaluate_positioned_label(clip_time, info.position, label, resolution)
    }

    /// Evaluate common styling and size each resolved metadata line.
    pub fn evaluate_metadata_for_render(
        &self,
        clip_time: TimelineTime,
        lines: impl IntoIterator<Item = (TitleMetadataLine, String)>,
        resolution: crate::Resolution,
    ) -> Result<EvaluatedBasicTitle> {
        let mut evaluated = self.evaluate(clip_time)?;
        evaluated.vertical_align = BasicTitleVerticalAlign::Top;
        for (line, label) in lines {
            if label.len() > BASIC_TITLE_MAX_TEXT_BYTES {
                return Err(title_error("Metadata label exceeds the title text budget"));
            }
            evaluated.positioned_lines.push(PositionedTitleLine {
                text: label,
                anchor: [
                    line.position[0] * resolution.width as f32,
                    line.position[1] * resolution.height as f32,
                ],
                font_size: evaluated.font_size * (line.scale_percent / 100.0),
            });
        }
        Ok(evaluated)
    }

    fn evaluate_positioned_label(
        &self,
        clip_time: TimelineTime,
        position: [f32; 2],
        label: &str,
        resolution: crate::Resolution,
    ) -> Result<EvaluatedBasicTitle> {
        if label.len() > BASIC_TITLE_MAX_TEXT_BYTES {
            return Err(title_error("Metadata label exceeds the title text budget"));
        }
        let mut evaluated = self.evaluate(clip_time)?;
        evaluated.text = label.to_owned();
        evaluated.layout_anchor = Some([
            position[0] * resolution.width as f32,
            position[1] * resolution.height as f32,
        ]);
        evaluated.vertical_align = BasicTitleVerticalAlign::Top;
        Ok(evaluated)
    }

    pub fn numbers_format(&self) -> Result<Option<TitleNumbersFormat>> {
        matches!(self.content_kind, BasicTitleContentKind::Numbers)
            .then(|| TitleNumbersFormat::read_from(&self.properties))
            .transpose()
    }

    /// Whether this title renders a generated timecode label.
    pub const fn is_timecode(&self) -> bool {
        matches!(self.content_kind, BasicTitleContentKind::Timecode)
    }

    /// Author settings for a generated Timecode Title, when applicable.
    pub fn timecode(&self) -> Result<Option<TitleTimecode>> {
        self.is_timecode()
            .then(|| TitleTimecode::read_from(&self.properties))
            .transpose()
    }

    /// Media placement whose original picture is excluded by this timecode overlay.
    pub fn timecode_original_media_to_hide(&self) -> Result<Option<crate::ClipId>> {
        if !self.is_timecode()
            || timecode::flag(&self.properties, Self::TIMECODE_COMPOSITE_ORIGINAL_PATH)?
        {
            return Ok(None);
        }
        match timecode::media_reference(&self.properties)? {
            crate::VisualSourceReference::Clip { clip_id, .. } => Ok(Some(clip_id)),
            _ => Ok(None),
        }
    }

    /// Evaluate style on the Clip clock and generate a label on the selected clock.
    pub fn evaluate_for_render(
        &self,
        clip_time: TimelineTime,
        sequence_time: TimelineTime,
        sequence_rate: crate::Rational,
        sequence_display: crate::TimelineDisplaySettings,
        media_sample: Option<TitleMediaTimecodeSample>,
    ) -> Result<EvaluatedBasicTitle> {
        if matches!(
            self.content_kind,
            BasicTitleContentKind::ClipInfo | BasicTitleContentKind::MetadataBurnIn
        ) {
            return Err(title_error(
                "Metadata title rendering requires sampled fields",
            ));
        }
        let mut evaluated = self.evaluate(clip_time)?;
        if let Some(timecode) = self.timecode()? {
            evaluated.text = timecode
                .label_with_media_sample(
                    sequence_time,
                    clip_time,
                    sequence_rate,
                    sequence_display,
                    media_sample,
                )
                .map_err(|error| title_error(error.to_string()))?;
            evaluated.layout_anchor = Some(timecode.position);
            evaluated.horizontal_align = BasicTitleHorizontalAlign::Left;
            evaluated.vertical_align = BasicTitleVerticalAlign::Top;
            if !timecode::show_box(&self.properties)? {
                evaluated.appearance.background_opacity = 0.0;
            }
        } else if self.content_kind == BasicTitleContentKind::Numbers {
            evaluated.text =
                numbers::label(&self.properties, clip_time, sequence_rate, sequence_display)?;
        }
        Ok(evaluated)
    }

    /// Create fresh canonical decoration properties for versioned legacy project migration.
    /// A caller must refuse to overwrite any decoration already present in an older payload.
    pub fn decoration_defaults() -> PropertyBag {
        let mut properties = PropertyBag::default();
        for descriptor in decoration_descriptors(BasicTitleContentKind::Text) {
            properties.define(descriptor);
        }
        properties
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
        let mut candidate = self.clone();
        candidate.properties.apply_mutation(mutation)?;
        candidate.validate_author_state()?;
        *self = candidate;
        Ok(())
    }

    /// Fork placement-local animation and keyframe identities.
    pub fn fork_author_identities(&mut self) {
        self.properties.fork_author_identities();
    }

    /// Retarget a stored media binding when its placement is copied with the title.
    pub fn remap_timecode_media_clip(
        &mut self,
        resolve: impl FnOnce(crate::ClipId) -> Option<crate::ClipId>,
    ) -> Result<()> {
        let Some(PropertyValue::VisualSource(crate::VisualSourceReference::Clip {
            clip_id,
            retime,
        })) = self.properties.evaluate(Self::TIMECODE_MEDIA_CLIP_PATH, TimelineTime::ZERO)
        else {
            return Ok(());
        };
        if let Some(clip_id) = resolve(clip_id) {
            self.properties.set_static_value(
                Self::TIMECODE_MEDIA_CLIP_PATH,
                PropertyValue::VisualSource(crate::VisualSourceReference::Clip { clip_id, retime }),
            )?;
        }
        Ok(())
    }

    /// Validate the complete persisted author contract.
    pub fn validate_author_state(&self) -> Result<()> {
        self.properties.validate()?;
        let canonical = canonical_descriptors(self.content_kind);
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

        if let Some(timecode) = self.timecode()? {
            timecode.validate()?;
        }
        if self.content_kind == BasicTitleContentKind::Numbers {
            numbers::validate(&self.properties)?;
        }
        self.clip_info()?;
        self.metadata_lines()?;
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
            layout_anchor: None,
            positioned_lines: Vec::new(),
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
            appearance: BasicTitleAppearance {
                stroke_width: required_float(&self.properties, Self::STROKE_WIDTH_PATH, time)?,
                stroke_color: required_color(&self.properties, Self::STROKE_COLOR_PATH, time)?,
                shadow_distance: required_float(
                    &self.properties,
                    Self::SHADOW_DISTANCE_PATH,
                    time,
                )?,
                shadow_softness: required_float(
                    &self.properties,
                    Self::SHADOW_SOFTNESS_PATH,
                    time,
                )?,
                shadow_color: required_color(&self.properties, Self::SHADOW_COLOR_PATH, time)?,
                background_color: required_color(
                    &self.properties,
                    Self::BACKGROUND_COLOR_PATH,
                    time,
                )?,
                background_opacity: required_float(
                    &self.properties,
                    Self::BACKGROUND_OPACITY_PATH,
                    time,
                )?,
                opacity: required_float(&self.properties, Self::OPACITY_PATH, time)?,
            },
        })
    }
}

fn canonical_descriptors(content_kind: BasicTitleContentKind) -> Vec<PropertyDescriptor> {
    let (font_default, font_min, font_max) = match content_kind {
        BasicTitleContentKind::Text => (96.0, 1.0, 4096.0),
        BasicTitleContentKind::Timecode => (36.0, 8.0, 128.0),
        BasicTitleContentKind::Numbers => (72.0, 1.0, 1000.0),
        BasicTitleContentKind::ClipInfo => (120.0, 1.0, 4096.0),
        BasicTitleContentKind::MetadataBurnIn => (15.0, 1.0, 1000.0),
    };
    let mut descriptors = vec![
        PropertyDescriptor::new(
            BasicTitle::TEXT_PATH,
            "文字",
            PropertyValue::Text("Title".to_owned()),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.text"))
        .with_text_mode(crate::ParameterTextMode::MultiLine)
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
        .with_enum_options(enum_options(&[
            ("normal", "Regular"),
            ("italic", "Italic"),
            ("oblique", "Oblique"),
        ]))
        .with_animatable(false),
        PropertyDescriptor::new(
            BasicTitle::FONT_SIZE_PATH,
            "字号",
            PropertyValue::Float(font_default),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.font_size"))
        .with_numeric_contract(
            ParameterUnit::Pixels,
            ParameterNumericContract::closed(
                font_min,
                font_max,
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
        .with_enum_options(enum_options(&[
            ("left", "Left"),
            ("center", "Center"),
            ("right", "Right"),
        ]))
        .with_animatable(false),
        PropertyDescriptor::new(
            BasicTitle::VERTICAL_ALIGN_PATH,
            "垂直对齐",
            PropertyValue::Enum("center".to_owned()),
        )
        .with_parameter_id(ParameterId::new_static("mondrian.title.vertical_align"))
        .with_enum_options(enum_options(&[
            ("top", "Top"),
            ("center", "Center"),
            ("bottom", "Bottom"),
        ]))
        .with_animatable(false),
    ];
    descriptors.extend(decoration_descriptors(content_kind));
    match content_kind {
        BasicTitleContentKind::Text => {}
        BasicTitleContentKind::Timecode => descriptors.extend(timecode::descriptors()),
        BasicTitleContentKind::Numbers => descriptors.extend(numbers::descriptors()),
        BasicTitleContentKind::ClipInfo => descriptors.extend(clip_info::descriptors()),
        BasicTitleContentKind::MetadataBurnIn => {
            descriptors.extend(metadata_burn_in::descriptors())
        }
    }
    descriptors
}

fn decoration_descriptors(content_kind: BasicTitleContentKind) -> [PropertyDescriptor; 8] {
    let defaults = BasicTitleAppearance::default();
    let pixels = |value, max| {
        (
            PropertyValue::Float(value),
            ParameterUnit::Pixels,
            ParameterNumericContract::closed(
                0.0,
                max,
                Some(1.0),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("Basic Title pixels constants form a valid numeric contract"),
        )
    };
    let opacity = |value| {
        (
            PropertyValue::Float(value),
            ParameterUnit::Normalized,
            ParameterNumericContract::closed(
                0.0,
                1.0,
                Some(0.01),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("Basic Title opacity constants form a valid numeric contract"),
        )
    };
    let numeric = |path, id, label, (value, unit, contract)| {
        PropertyDescriptor::new(path, label, value)
            .with_parameter_id(ParameterId::new_static(id))
            .with_numeric_contract(unit, contract)
            .with_animatable(false)
    };
    let color = |path, id, label, value| {
        PropertyDescriptor::new(path, label, PropertyValue::Color(value))
            .with_parameter_id(ParameterId::new_static(id))
            .with_animatable(false)
    };
    [
        numeric(
            BasicTitle::STROKE_WIDTH_PATH,
            "mondrian.title.stroke_width",
            "描边宽度",
            pixels(defaults.stroke_width, 50.0),
        ),
        color(
            BasicTitle::STROKE_COLOR_PATH,
            "mondrian.title.stroke_color",
            "描边颜色",
            defaults.stroke_color,
        ),
        numeric(
            BasicTitle::SHADOW_DISTANCE_PATH,
            "mondrian.title.shadow_distance",
            "阴影距离",
            pixels(defaults.shadow_distance, 100.0),
        ),
        numeric(
            BasicTitle::SHADOW_SOFTNESS_PATH,
            "mondrian.title.shadow_softness",
            "阴影柔化",
            pixels(defaults.shadow_softness, 100.0),
        ),
        color(
            BasicTitle::SHADOW_COLOR_PATH,
            "mondrian.title.shadow_color",
            "阴影颜色",
            defaults.shadow_color,
        ),
        color(
            BasicTitle::BACKGROUND_COLOR_PATH,
            "mondrian.title.background_color",
            "背景颜色",
            defaults.background_color,
        ),
        numeric(
            BasicTitle::BACKGROUND_OPACITY_PATH,
            "mondrian.title.background_opacity",
            "背景不透明度",
            opacity(if content_kind == BasicTitleContentKind::Timecode {
                1.0
            } else {
                defaults.background_opacity
            }),
        ),
        numeric(
            BasicTitle::OPACITY_PATH,
            "mondrian.title.opacity",
            "标题不透明度",
            opacity(
                if matches!(
                    content_kind,
                    BasicTitleContentKind::ClipInfo | BasicTitleContentKind::MetadataBurnIn
                ) {
                    0.4
                } else {
                    defaults.opacity
                },
            ),
        ),
    ]
}

fn enum_options(choices: &[(&str, &str)]) -> Vec<ParameterEnumOption> {
    choices
        .iter()
        .map(|(key, caption)| {
            ParameterEnumOption::new(*key, *caption, format!("mondrian.title.option.{key}"))
        })
        .collect()
}

/// Default concrete font dependency for authored Basic Titles on this platform.
///
/// Linux uses a declared CJK family that covers the default Chinese title text.
/// A missing family remains an explicit rasterization error, never an implicit
/// system-font fallback.
pub const fn default_basic_title_font_family() -> &'static str {
    if cfg!(target_os = "windows") {
        "Microsoft YaHei"
    } else if cfg!(target_os = "macos") {
        // The software title generator requires standard outline tables.
        // macOS PingFang UI can expose hvgl-only faces to CoreText.
        "Heiti SC"
    } else if cfg!(target_os = "linux") {
        "Noto Sans CJK SC"
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
        let Self { properties, content_kind: _ } = self;
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
        assert_eq!(
            title.property_bag().iter().count(),
            BasicTitle::PROPERTY_PATHS.len()
        );
        assert_eq!(evaluated.appearance, BasicTitleAppearance::default());
    }

    #[test]
    fn title_decoration_values_are_static_typed_and_transactional() {
        let mut title = BasicTitle::new("Decorated", default_basic_title_font_family()).unwrap();
        let color = Color { r: 2.0, g: 0.5, b: 0.25, a: 0.25 };
        for (path, value) in [
            (BasicTitle::STROKE_WIDTH_PATH, PropertyValue::Float(50.0)),
            (BasicTitle::STROKE_COLOR_PATH, PropertyValue::Color(color)),
            (BasicTitle::OPACITY_PATH, PropertyValue::Float(0.25)),
        ] {
            title
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: path.into(),
                    value,
                })
                .unwrap();
        }
        let appearance = title.evaluate(tt(23)).unwrap().appearance;
        assert_eq!(
            (
                appearance.stroke_width,
                appearance.stroke_color,
                appearance.opacity
            ),
            (50.0, color, 0.25)
        );
        let before = title.clone();
        for (path, value) in [
            (BasicTitle::STROKE_WIDTH_PATH, PropertyValue::Float(50.01)),
            (
                BasicTitle::SHADOW_SOFTNESS_PATH,
                PropertyValue::Float(f32::NAN),
            ),
            (BasicTitle::BACKGROUND_COLOR_PATH, PropertyValue::Float(1.0)),
            (BasicTitle::OPACITY_PATH, PropertyValue::Float(-0.1)),
            (BasicTitle::FONT_FAMILY_PATH, PropertyValue::Text("".into())),
        ] {
            assert!(title
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: path.into(),
                    value
                })
                .is_err());
            assert_eq!(title, before);
        }
        assert!(title
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: BasicTitle::OPACITY_PATH.into(),
                keyframe: Keyframe::linear(tt(0), PropertyValue::Float(0.5)),
            })
            .is_err());
        assert_eq!(title, before);
    }

    #[test]
    fn title_requires_decorations_in_current_schema() {
        let title = BasicTitle::new("Decorated", default_basic_title_font_family()).unwrap();
        for (path, _) in BasicTitle::decoration_defaults().iter() {
            let mut raw = serde_json::to_value(&title).unwrap();
            raw["properties"]["properties"].as_object_mut().unwrap().remove(path).unwrap();
            let truncated: BasicTitle = serde_json::from_value(raw).unwrap();
            assert!(truncated.validate_author_state().is_err(), "missing {path}");
        }
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
