//! App-owned message formatting; authoring state persists stable IDs only.

use std::sync::{Arc, OnceLock};

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
use mondrian_core::effect_data::EffectType;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use unic_langid::LanguageIdentifier;

const ZH_CN_CATALOG: &str = include_str!("../../locales/zh-CN.ftl");
const EN_US_CATALOG: &str = include_str!("../../locales/en-US.ftl");
static ZH_CN_RESOURCE: OnceLock<Result<Arc<FluentResource>, String>> = OnceLock::new();
static PSEUDO_RESOURCE: OnceLock<Result<Arc<FluentResource>, String>> = OnceLock::new();
static EN_US_RESOURCE: OnceLock<Result<Arc<FluentResource>, String>> = OnceLock::new();

/// A supported product UI locale, independent of Project language metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppUiLocale {
    /// Simplified Chinese, also the complete product fallback.
    ZhCn,
    /// US English.
    EnUs,
    /// Expanded Chinese output for layout and clipping checks.
    Pseudo,
}

impl AppUiLocale {
    fn catalog(self) -> &'static str {
        match self {
            Self::ZhCn | Self::Pseudo => ZH_CN_CATALOG,
            Self::EnUs => EN_US_CATALOG,
        }
    }

    fn language_tag(self) -> &'static str {
        match self {
            Self::ZhCn | Self::Pseudo => "zh-CN",
            Self::EnUs => "en-US",
        }
    }
}

/// Machine-local choice; System resolves once when constructing a UI model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppUiLocalePreference {
    /// Use the first supported language matching the desktop locale.
    #[default]
    System,
    /// Always show Simplified Chinese.
    ZhCn,
    /// Always show US English.
    EnUs,
    /// Expand translated text to expose insufficient layout space.
    Pseudo,
}

impl AppUiLocalePreference {
    /// Resolve the one UI locale from a captured system BCP 47 tag.
    pub fn resolve(self, system_tag: Option<&str>) -> AppUiLocale {
        match self {
            Self::ZhCn => AppUiLocale::ZhCn,
            Self::EnUs => AppUiLocale::EnUs,
            Self::Pseudo => AppUiLocale::Pseudo,
            Self::System => match system_tag {
                Some(tag)
                    if tag
                        .split(['-', '_'])
                        .next()
                        .is_some_and(|language| language.eq_ignore_ascii_case("en")) =>
                {
                    AppUiLocale::EnUs
                }
                _ => AppUiLocale::ZhCn,
            },
        }
    }
}

/// A malformed bundled catalog or invalid product locale identity.
#[derive(Debug, Error)]
pub enum LocalizationError {
    /// A product-owned locale tag cannot be parsed.
    #[error("invalid product locale tag: {0}")]
    InvalidLocale(String),
    /// A bundled catalog failed Fluent parsing or registration.
    #[error("invalid bundled Fluent catalog for {locale}: {reason}")]
    InvalidCatalog {
        locale: &'static str,
        reason: String,
    },
}

/// Immutable formatter for one UI locale snapshot.
pub struct Localizer {
    locale: AppUiLocale,
    requested: FluentBundle<Arc<FluentResource>>,
    fallback: FluentBundle<Arc<FluentResource>>,
}

impl Localizer {
    /// Parse built-in catalogs and validate their message registration.
    pub fn new(locale: AppUiLocale) -> Result<Self, LocalizationError> {
        Ok(Self {
            locale,
            requested: build_bundle(locale)?,
            fallback: build_bundle(AppUiLocale::ZhCn)?,
        })
    }

    /// Locale captured by this formatter.
    pub const fn locale(&self) -> AppUiLocale {
        self.locale
    }

    /// Format one message without arguments.
    pub fn text(&self, message_id: &str) -> String {
        self.format(message_id, None)
    }

    /// Format one message with named Fluent arguments. Missing or invalid
    /// requested messages fall back to Chinese, then to the stable message ID.
    pub fn format(&self, message_id: &str, args: Option<&FluentArgs<'_>>) -> String {
        format_from(&self.requested, message_id, args)
            .or_else(|| format_from(&self.fallback, message_id, args))
            .unwrap_or_else(|| message_id.to_owned())
    }

    /// Resolve a parameter presentation address without changing its author key.
    /// Unknown vendor addresses retain their declared display fallback.
    pub(crate) fn parameter_text(&self, address: &str, fallback: &str) -> String {
        let direct = format_from(&self.requested, address, None)
            .or_else(|| format_from(&self.fallback, address, None));
        if let Some(text) = direct {
            return text;
        }
        if address.starts_with("mondrian.title.")
            || address.starts_with("mondrian.mask.")
            || address.starts_with("mondrian.effect.builtin.")
            || address.starts_with("mondrian.effect.curves.")
            || address.starts_with("mondrian.effect.qualifier.")
            || address.starts_with("builtin.lut_3d.")
        {
            let message_id = format!("parameter-{}", address.replace(['.', '_'], "-"));
            return format_from(&self.requested, &message_id, None)
                .or_else(|| format_from(&self.fallback, &message_id, None))
                .unwrap_or_else(|| fallback.to_owned());
        }
        fallback.to_owned()
    }

    /// Localize a built-in group heading while retaining its definition identity.
    pub(crate) fn parameter_group_text(&self, parameter_id: &str, group: &str) -> String {
        if parameter_id.starts_with("mondrian.effect.builtin.")
            && let Some(message_id) = builtin_parameter_group_message_id(group)
        {
            return self.text(message_id);
        }
        group.to_owned()
    }

    /// Resolve language-neutral status or notification data in this UI locale.
    pub fn message(&self, message: &crate::app::AppMessage) -> String {
        use crate::app::AppMessageValue;
        let mut args = FluentArgs::new();
        for (name, value) in &message.args {
            match value {
                AppMessageValue::Number(value) if value.unsigned_abs() <= (1_u64 << 53) => {
                    args.set(*name, *value as f64);
                }
                AppMessageValue::Number(value) => args.set(*name, value.to_string()),
                AppMessageValue::Text(value) => args.set(*name, value.as_str()),
                AppMessageValue::Message(value) => args.set(*name, self.message(value)),
            }
        }
        self.format(message.id, Some(&args))
    }

    /// Format one display-only text argument for a UI message.
    pub(crate) fn format_text(&self, message_id: &str, argument: &str, value: &str) -> String {
        let mut args = FluentArgs::new();
        args.set(argument, value);
        self.format(message_id, Some(&args))
    }
}

/// Render retained product facts explicitly for Chinese UI assertions in App tests.
#[cfg(test)]
pub(crate) fn chinese_product_message(message: &crate::app::AppMessage) -> String {
    Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog").message(message)
}

/// App-owned caption identity for a typed keyframe interpolation mode.
pub(crate) fn keyframe_interpolation_message_id(
    interpolation: mondrian_core::automation::InterpolationType,
) -> &'static str {
    use mondrian_core::automation::InterpolationType;
    match interpolation {
        InterpolationType::Hold => "keyframe-interpolation-hold",
        InterpolationType::Linear => "keyframe-interpolation-linear",
        InterpolationType::Bezier => "keyframe-interpolation-bezier",
        InterpolationType::AutoBezier => "keyframe-interpolation-auto-bezier",
        InterpolationType::ContinuousBezier => "keyframe-interpolation-continuous-bezier",
        InterpolationType::EaseIn => "keyframe-interpolation-ease-in",
        InterpolationType::EaseOut => "keyframe-interpolation-ease-out",
    }
}

/// Shared product message ID for the exact Export lifecycle phase.
pub(crate) fn export_progress_phase_message_id(
    phase: mondrian_export::ExportProgressPhase,
) -> &'static str {
    use mondrian_export::ExportProgressPhase;
    match phase {
        ExportProgressPhase::Preparing => "export-job-preparing",
        ExportProgressPhase::Rendering => "export-job-rendering",
        ExportProgressPhase::Encoding => "export-job-encoding",
        ExportProgressPhase::Packaging => "export-job-packaging",
        ExportProgressPhase::Validating => "export-job-validating",
        ExportProgressPhase::Publishing => "export-job-publishing",
    }
}

/// Stable UI message key for a built-in visual effect.
pub(crate) fn builtin_effect_message_id(effect_type: &EffectType) -> Option<String> {
    effect_type
        .key()
        .strip_prefix("builtin.")
        .map(|key| format!("effect-{}", key.replace('_', "-")))
}

/// Category display key; the category path itself remains a stable tree node ID.
pub(crate) fn effect_category_message_id(category: &str) -> Option<&'static str> {
    match category {
        "颜色" => Some("effect-category-color"),
        "调色" => Some("effect-category-grading"),
        "模糊与锐化" => Some("effect-category-blur-sharpen"),
        "风格化" => Some("effect-category-stylize"),
        "变换" => Some("effect-category-transform"),
        "抠像" => Some("effect-category-keying"),
        "插件" => Some("effect-category-plugins"),
        _ => None,
    }
}

fn builtin_parameter_group_message_id(group: &str) -> Option<&'static str> {
    match group {
        "ASC CDL" => Some("parameter-group-asc-cdl"),
        "HDR · Blacks" => Some("parameter-group-hdr-blacks"),
        "HDR · Dark" => Some("parameter-group-hdr-dark"),
        "HDR · Global" => Some("parameter-group-hdr-global"),
        "HDR · Highlights" => Some("parameter-group-hdr-highlights"),
        "HDR · Light" => Some("parameter-group-hdr-light"),
        "HDR · Shadows" => Some("parameter-group-hdr-shadows"),
        "HDR · Specular" => Some("parameter-group-hdr-specular"),
        "HSL" => Some("parameter-group-hsl"),
        "LUT" => Some("parameter-group-lut"),
        "Primaries" => Some("parameter-group-primaries"),
        "Qualifier" => Some("parameter-group-qualifier"),
        "Qualifier · 3D" => Some("parameter-group-qualifier-3d"),
        "Qualifier · Matte" => Some("parameter-group-qualifier-matte"),
        "亮度键" => Some("parameter-group-luma-key"),
        "基础校正" => Some("parameter-group-basic-correction"),
        "抠像" => Some("parameter-group-chroma-key"),
        "暗角" => Some("parameter-group-vignette"),
        "曲线" => Some("parameter-group-curves"),
        "模糊" => Some("parameter-group-blur"),
        "白平衡" => Some("parameter-group-white-balance"),
        "色域压缩" => Some("parameter-group-gamut-compression"),
        "色差" => Some("parameter-group-chromatic-aberration"),
        "裁切" => Some("parameter-group-crop"),
        "锐化" => Some("parameter-group-sharpen"),
        "颗粒" => Some("parameter-group-grain"),
        "高光恢复" => Some("parameter-group-highlight-recovery"),
        _ => None,
    }
}

fn build_bundle(
    locale: AppUiLocale,
) -> Result<FluentBundle<Arc<FluentResource>>, LocalizationError> {
    let tag = locale.language_tag();
    let langid: LanguageIdentifier =
        tag.parse().map_err(|_| LocalizationError::InvalidLocale(tag.to_owned()))?;
    let cached = match locale {
        AppUiLocale::ZhCn => &ZH_CN_RESOURCE,
        AppUiLocale::Pseudo => &PSEUDO_RESOURCE,
        AppUiLocale::EnUs => &EN_US_RESOURCE,
    };
    let resource = cached
        .get_or_init(|| {
            let catalog = if locale == AppUiLocale::Pseudo {
                pseudo_catalog(locale.catalog())?
            } else {
                locale.catalog().to_owned()
            };
            FluentResource::try_new(catalog)
                .map(Arc::new)
                .map_err(|(_, errors)| format!("{errors:?}"))
        })
        .as_ref()
        .map_err(|reason| LocalizationError::InvalidCatalog {
            locale: tag,
            reason: reason.clone(),
        })?;
    let mut bundle = FluentBundle::new(vec![langid]);
    bundle
        .add_resource(resource.clone())
        .map_err(|errors| LocalizationError::InvalidCatalog {
            locale: tag,
            reason: format!("{errors:?}"),
        })?;
    Ok(bundle)
}

fn format_from(
    bundle: &FluentBundle<Arc<FluentResource>>,
    message_id: &str,
    args: Option<&FluentArgs<'_>>,
) -> Option<String> {
    let pattern = bundle.get_message(message_id)?.value()?;
    let mut errors = Vec::new();
    let text = bundle.format_pattern(pattern, args, &mut errors);
    errors.is_empty().then(|| text.into_owned())
}

// Expand catalog literals once; runtime arguments and Fluent expression
// identities remain exact, including nested already-localized messages.
fn pseudo_catalog(catalog: &str) -> Result<String, String> {
    use fluent_syntax::ast::Entry;
    let mut resource = fluent_syntax::parser::parse(catalog.to_owned())
        .map_err(|(_, errors)| format!("{errors:?}"))?;
    for entry in &mut resource.body {
        match entry {
            Entry::Message(message) => {
                if let Some(value) = &mut message.value {
                    pseudo_pattern(value, true);
                }
                for attribute in &mut message.attributes {
                    pseudo_pattern(&mut attribute.value, true);
                }
            }
            Entry::Term(term) => {
                pseudo_pattern(&mut term.value, false);
                for attribute in &mut term.attributes {
                    pseudo_pattern(&mut attribute.value, false);
                }
            }
            _ => {}
        }
    }
    Ok(fluent_syntax::serializer::serialize(&resource))
}

fn pseudo_pattern(pattern: &mut fluent_syntax::ast::Pattern<String>, boundaries: bool) {
    use fluent_syntax::ast::PatternElement;
    for element in &mut pattern.elements {
        match element {
            PatternElement::TextElement { value } => {
                let mut expanded = String::with_capacity(value.len().saturating_mul(2));
                for character in value.chars() {
                    expanded.push(character);
                    if character.is_alphabetic() {
                        expanded.push('·');
                    }
                }
                *value = expanded;
            }
            PatternElement::Placeable { expression } => pseudo_expression(expression),
        }
    }
    if boundaries {
        pattern
            .elements
            .insert(0, PatternElement::TextElement { value: "⟦".to_owned() });
        pattern.elements.push(PatternElement::TextElement { value: "⟧".to_owned() });
    }
}

fn pseudo_expression(expression: &mut fluent_syntax::ast::Expression<String>) {
    use fluent_syntax::ast::{Expression, InlineExpression};
    match expression {
        Expression::Select { variants, .. } => {
            for variant in variants {
                pseudo_pattern(&mut variant.value, false);
            }
        }
        Expression::Inline(InlineExpression::Placeable { expression }) => {
            pseudo_expression(expression)
        }
        Expression::Inline(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parameter_presentations_cover_registered_schemas() {
        use std::collections::BTreeMap;
        let mut declared = BTreeMap::new();
        for kind in mondrian_effects::effect_library_types() {
            if !kind.key().starts_with("builtin.") {
                continue;
            }
            let effect = mondrian_effects::instantiate_effect_node(kind).expect("builtin schema");
            for (_, property) in effect.properties.iter() {
                declared.insert(
                    property.descriptor.schema.message_id.clone(),
                    (
                        property.descriptor.display_name.clone(),
                        property.descriptor.ui_metadata.group_name.clone(),
                    ),
                );
                for option in &property.descriptor.schema.enum_options {
                    declared.insert(
                        option.message_id.clone(),
                        (option.display_name.clone(), None),
                    );
                }
            }
        }
        let mut presentation_addresses = BTreeMap::new();
        for address in declared.keys() {
            let message_id = format!("parameter-{}", address.replace(['.', '_'], "-"));
            assert!(
                presentation_addresses.insert(message_id, address).is_none(),
                "parameter presentation address collision: {address}"
            );
        }
        for locale in [AppUiLocale::EnUs, AppUiLocale::ZhCn, AppUiLocale::Pseudo] {
            let localizer = Localizer::new(locale).expect("catalog");
            assert_eq!(
                localizer.parameter_text("vendor.custom.mode", "Vendor mode"),
                "Vendor mode"
            );
            let missing = declared
                .iter()
                .filter(|(address, _)| {
                    let message_id = format!("parameter-{}", address.replace(['.', '_'], "-"));
                    localizer
                        .requested
                        .get_message(&message_id)
                        .and_then(|message| message.value())
                        .is_none()
                })
                .collect::<BTreeMap<_, _>>();
            for (address, (_, group)) in &declared {
                if let Some(group) = group {
                    let message_id = builtin_parameter_group_message_id(group)
                        .expect("registered builtin group");
                    assert!(
                        localizer
                            .requested
                            .get_message(message_id)
                            .and_then(|message| message.value())
                            .is_some(),
                        "missing {locale:?} group {message_id}"
                    );
                    assert_eq!(
                        localizer.parameter_group_text(address, group),
                        localizer.text(message_id)
                    );
                }
            }
            assert_eq!(
                localizer.parameter_group_text("vendor.param", "曲线"),
                "曲线"
            );
            assert!(
                missing.is_empty(),
                "missing registered parameter presentations for {locale:?}: {}",
                serde_json::to_string(&missing).expect("missing presentation diagnostics")
            );
        }
    }

    #[test]
    fn bundled_catalogs_have_identical_message_ids() {
        fn ids(catalog: &str) -> std::collections::BTreeSet<&str> {
            catalog
                .lines()
                .filter_map(|line| {
                    let (key, _) = line.split_once('=')?;
                    let key = key.trim();
                    (!key.is_empty()
                        && key.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                        }))
                    .then_some(key)
                })
                .collect()
        }

        let chinese = ids(ZH_CN_CATALOG);
        let english = ids(EN_US_CATALOG);
        assert!(
            chinese.len() > 400,
            "product catalog unexpectedly truncated"
        );
        assert_eq!(chinese, english, "a product string lacks one translation");
    }

    #[test]
    fn blend_mode_catalogs_cover_every_typed_option() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        let row_id = "inspector-blend-mode";
        assert!(
            chinese.requested.get_message(row_id).is_some(),
            "missing zh-CN: {row_id}"
        );
        assert!(
            english.requested.get_message(row_id).is_some(),
            "missing en-US: {row_id}"
        );
        for option in mondrian_core::display_labels::blend_mode_options() {
            let id = format!("blend-mode-{}", option.value.to_ascii_lowercase());
            assert!(
                chinese.requested.get_message(&id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(&id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn native_file_dialog_messages_exist_in_both_catalogs() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for id in [
            "file-dialog-create-project",
            "file-dialog-open-project",
            "file-dialog-import-media",
            "file-dialog-install-clap",
            "file-dialog-install-vst3",
            "file-dialog-install-vst3-bundle",
            "file-dialog-install-openfx-bundle",
            "file-dialog-relink-media",
            "file-dialog-save-as-project",
            "file-dialog-package-project",
            "file-dialog-export-output",
            "file-dialog-import-ancillary",
            "file-dialog-import-pse",
            "file-dialog-select-icc",
            "file-default-untitled",
            "file-default-untitled-project",
            "file-filter-project",
            "file-filter-package",
            "file-filter-video",
            "file-filter-image",
            "file-filter-audio",
            "file-filter-clap",
            "file-filter-vst3",
            "file-filter-media",
            "file-filter-export",
            "file-filter-ancillary",
            "file-filter-pse",
            "file-filter-icc",
        ] {
            assert!(
                chinese.requested.get_message(id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn startup_recovery_messages_exist_in_both_catalogs() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for id in [
            "recovery-title",
            "recovery-summary",
            "recovery-time",
            "recovery-saved-at",
            "recovery-source",
            "recovery-target",
            "recovery-target-state",
            "recovery-target-missing",
            "recovery-target-same",
            "recovery-target-older",
            "recovery-target-newer",
            "recovery-safety",
            "recovery-confirm",
            "recovery-cancel",
            "recovery-age-seconds",
            "recovery-age-minutes",
            "recovery-age-hours",
            "recovery-age-days",
            "recovery-row-single",
            "recovery-row-multiple",
            "startup-recent-now",
            "startup-recent-unknown-time",
            "startup-recent-unknown-size",
            "startup-recent-file-unavailable",
            "startup-recent-detail",
        ] {
            assert!(
                chinese.requested.get_message(id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn pending_close_messages_exist_in_both_catalogs() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for id in [
            "pending-close-title",
            "pending-close-body-close",
            "pending-close-body-quit",
            "pending-close-save-close",
            "pending-close-save-quit",
            "pending-close-discard",
            "pending-close-cancel",
        ] {
            assert!(
                chinese.requested.get_message(id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn sequence_settings_messages_exist_in_both_catalogs() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for id in [
            "sequence-apply",
            "sequence-audio",
            "sequence-audio-discrete",
            "sequence-audio-milliseconds",
            "sequence-audio-mono",
            "sequence-audio-samples",
            "sequence-audio-speakers",
            "sequence-audio-stereo",
            "sequence-auto-tone-map",
            "sequence-cancel",
            "sequence-color-display-referred",
            "sequence-color-management",
            "sequence-color-scene-referred",
            "sequence-custom-frame-size",
            "sequence-display-frames",
            "sequence-edit-custom",
            "sequence-field-lower",
            "sequence-field-progressive",
            "sequence-field-upper",
            "sequence-format",
            "sequence-height",
            "sequence-metadata-assume-709",
            "sequence-metadata-reject",
            "sequence-name",
            "sequence-name-placeholder",
            "sequence-name-required",
            "sequence-pixel-square",
            "sequence-pixel-unknown",
            "sequence-preview",
            "sequence-preview-cache",
            "sequence-preview-resolution",
            "sequence-project-color-engine",
            "sequence-range-full",
            "sequence-range-legal",
            "sequence-resolution-fhd",
            "sequence-resolution-hd",
            "sequence-settings-description",
            "sequence-settings-title",
            "sequence-start-frame",
            "sequence-static-hdr",
            "sequence-tab-color",
            "sequence-tab-format",
            "sequence-tab-preview",
            "sequence-timecode-start",
            "sequence-tone-map-always",
            "sequence-tone-map-auto",
            "sequence-tone-map-never",
            "sequence-width",
        ] {
            assert!(
                chinese.requested.get_message(id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn startup_messages_exist_in_both_catalogs() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for id in [
            "startup-heading",
            "startup-new-project",
            "startup-open-project",
            "startup-recoverable-projects",
            "startup-recent-projects",
            "startup-no-recent-projects",
        ] {
            assert!(
                chinese.requested.get_message(id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn new_project_messages_exist_in_both_catalogs() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for id in [
            "new-project-title",
            "new-project-untitled",
            "new-project-description",
            "new-project-name",
            "new-project-name-placeholder",
            "new-project-frame-size",
            "new-project-frame-rate",
            "new-project-audio",
            "new-project-color-mode",
            "new-project-resolution-hd",
            "new-project-resolution-fhd",
            "color-custom-ocio",
            "color-select-custom-ocio",
            "new-project-create-proxies",
            "new-project-preview-cache",
            "new-project-cancel",
            "new-project-create",
            "color-choose-ocio-config",
            "color-ocio-config-filter",
        ] {
            assert!(
                chinese.requested.get_message(id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn project_settings_messages_exist_in_both_catalogs() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for id in [
            "project-settings-title",
            "project-settings-description",
            "project-settings-builtin-detail",
            "project-settings-aces-detail",
            "project-settings-custom-detail",
            "project-settings-cancel",
            "project-settings-apply",
        ] {
            assert!(
                chinese.requested.get_message(id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn viewer_messages_exist_in_both_catalogs() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for id in [
            "panel-viewer",
            "viewer-no-sequence",
            "viewer-no-signal",
            "viewer-fit",
            "viewer-no-sequence-loaded",
            "viewer-frame-count",
            "viewer-loading",
            "viewer-color-rejected",
            "viewer-blocked",
            "viewer-failed",
            "viewer-playing",
            "viewer-ready",
            "viewer-color-rejection-detail",
        ] {
            assert!(
                chinese.requested.get_message(id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn asset_browser_messages_exist_in_both_catalogs() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for id in [
            "panel-assets",
            "asset-library",
            "asset-search",
            "asset-empty-title",
            "asset-empty-description",
            "asset-no-results-title",
            "asset-no-results-description",
            "asset-library-disconnected",
            "asset-library-unavailable",
            "asset-delete-selected",
            "asset-back",
            "asset-parent",
            "asset-all",
            "asset-all-badge",
            "asset-item-count",
            "asset-kind-video",
            "asset-kind-still",
            "asset-kind-audio",
            "asset-kind-adjustment",
            "asset-kind-solid",
            "asset-offline",
            "asset-proxy",
            "asset-interpret",
            "asset-reveal",
            "asset-relink",
            "asset-disable-proxy",
            "asset-enable-proxy",
            "asset-delete",
            "asset-delete-folder",
            "asset-import",
            "asset-new",
            "asset-new-adjustment",
            "asset-new-solid",
            "asset-new-folder",
        ] {
            assert!(
                chinese.requested.get_message(id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn every_builtin_effect_and_category_has_chinese_and_english_copy() {
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for effect_type in mondrian_effects::effect_library_types() {
            if let Some(id) = builtin_effect_message_id(&effect_type) {
                assert!(
                    chinese.requested.get_message(&id).is_some(),
                    "missing zh-CN: {id}"
                );
                assert!(
                    english.requested.get_message(&id).is_some(),
                    "missing en-US: {id}"
                );
            }
            for category in effect_type.category_path() {
                let id = effect_category_message_id(category).expect("known category key");
                assert!(
                    chinese.requested.get_message(id).is_some(),
                    "missing zh-CN: {id}"
                );
                assert!(
                    english.requested.get_message(id).is_some(),
                    "missing en-US: {id}"
                );
            }
        }
        for id in ["effect-empty", "effect-search"] {
            assert!(
                chinese.requested.get_message(id).is_some(),
                "missing zh-CN: {id}"
            );
            assert!(
                english.requested.get_message(id).is_some(),
                "missing en-US: {id}"
            );
        }
    }

    #[test]
    fn every_application_menu_row_has_chinese_and_english_copy() {
        use mondrian_ui_widgets::menu::{MenuItem, MenuItemKind};

        fn visit(items: &[MenuItem], chinese: &Localizer, english: &Localizer) {
            for item in items {
                if !matches!(item.kind, MenuItemKind::Separator) {
                    let id = item.message_id.as_deref().expect("application menu row key");
                    assert!(
                        chinese.requested.get_message(id).is_some(),
                        "missing zh-CN: {id}"
                    );
                    assert!(
                        english.requested.get_message(id).is_some(),
                        "missing en-US: {id}"
                    );
                }
                if let MenuItemKind::Submenu { children } = &item.kind {
                    visit(children, chinese, english);
                }
            }
        }

        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        for (_, items) in crate::app_ui::menu_bar::default_menu_items() {
            visit(&items, &chinese, &english);
        }
        use mondrian_core::automation::InterpolationType;
        for interpolation in [
            InterpolationType::Hold,
            InterpolationType::Linear,
            InterpolationType::Bezier,
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
            InterpolationType::EaseIn,
            InterpolationType::EaseOut,
        ] {
            let id = keyframe_interpolation_message_id(interpolation);
            visit(
                &[MenuItem::inert(chinese.text(id)).with_message_id(id)],
                &chinese,
                &english,
            );
            assert_ne!(
                chinese.text(id),
                english.text(id),
                "each keyframe mode must have localized copy"
            );
        }
    }

    #[test]
    fn locale_resolution_keeps_project_independent_and_falls_back() {
        assert_eq!(
            AppUiLocalePreference::System.resolve(Some("en-GB")),
            AppUiLocale::EnUs
        );
        assert_eq!(
            AppUiLocalePreference::System.resolve(Some("ja-JP")),
            AppUiLocale::ZhCn
        );
        assert_eq!(
            AppUiLocalePreference::System.resolve(Some("english")),
            AppUiLocale::ZhCn
        );
        assert_eq!(
            AppUiLocalePreference::ZhCn.resolve(Some("en-US")),
            AppUiLocale::ZhCn
        );
    }

    #[test]
    fn fluent_arguments_plural_selection_and_missing_fallback_work() {
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        let mut args = FluentArgs::new();
        args.set("count", 1);
        assert_eq!(
            english.format("notification-import-complete", Some(&args)),
            "Imported one media file"
        );
        args.set("count", 3);
        let plural = english.format("notification-import-complete", Some(&args));
        assert!(plural.contains('3') && plural.contains("media files"));
        let mut partial = FluentArgs::new();
        partial.set("imported", 1);
        partial.set("failed", 1);
        let partial_text = english.format("notification-import-partial", Some(&partial));
        assert!(partial_text.contains("one media file"));
        assert!(partial_text.contains("one failed"));
        assert_eq!(english.text("missing-message"), "missing-message");
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        assert!(chinese.format("notification-import-complete", Some(&args)).contains('3'));
    }

    #[test]
    fn exact_time_reasons_localize_without_losing_rational_coordinates() {
        use crate::app::AppMessage;
        use mondrian_core::{timeline_time::TimelineTimeError, MondrianError};
        for reason in [
            TimelineTimeError::ZeroDenominator,
            TimelineTimeError::Overflow,
            TimelineTimeError::InvalidLegacyTimeBase { numerator: i64::MIN, denominator: i64::MAX },
            TimelineTimeError::InvalidFrameRate { numerator: i64::MAX, denominator: i64::MIN },
            TimelineTimeError::DomainMismatch,
            TimelineTimeError::NonInvertibleTransform,
            TimelineTimeError::NegativeDuration,
            TimelineTimeError::NonFiniteInput,
        ] {
            let message = AppMessage::from_error(&MondrianError::InvalidTimelineTime(reason));
            let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese").message(&message);
            for locale in [AppUiLocale::ZhCn, AppUiLocale::EnUs, AppUiLocale::Pseudo] {
                let localizer = Localizer::new(locale).expect("catalog");
                let text = localizer.message(&message);
                assert!(!text.contains("reason-time-"));
                if locale != AppUiLocale::ZhCn {
                    assert_ne!(text, chinese);
                }
                if matches!(
                    reason,
                    TimelineTimeError::InvalidLegacyTimeBase { .. }
                        | TimelineTimeError::InvalidFrameRate { .. }
                ) {
                    assert!(text.contains(&i64::MIN.to_string()));
                    assert!(text.contains(&i64::MAX.to_string()));
                }
            }
        }
    }

    #[test]
    fn typed_core_failures_resolve_in_all_locales_without_parsing_diagnostics() {
        use crate::app::AppMessage;
        use mondrian_core::MondrianError;
        let failures = vec![
            MondrianError::MediaOpen {
                path: "native detail / {file}".to_owned(),
                reason: "native detail / {file}".to_owned(),
            },
            MondrianError::DecodeFailed {
                asset_id: "native detail / {file}".to_owned(),
                reason: "native detail / {file}".to_owned(),
            },
            MondrianError::UnsupportedFormat { format: "native detail / {file}".to_owned() },
            MondrianError::ProxyGenerationFailed { reason: "native detail / {file}".to_owned() },
            MondrianError::TrackNotFound { track_id: "native detail / {file}".to_owned() },
            MondrianError::ClipNotFound { clip_id: "native detail / {file}".to_owned() },
            MondrianError::TimecodeOutOfRange { timecode: "native detail / {file}".to_owned() },
            MondrianError::TrackLocked { track_id: "native detail / {file}".to_owned() },
            MondrianError::ActionNotExecuted {
                action: "native detail / {file}".to_owned(),
                reason: "native detail / {file}".to_owned(),
            },
            MondrianError::GpuInitFailed { reason: "native detail / {file}".to_owned() },
            MondrianError::ShaderCompileFailed {
                shader_name: "native detail / {file}".to_owned(),
                reason: "native detail / {file}".to_owned(),
            },
            MondrianError::TextureUploadFailed { reason: "native detail / {file}".to_owned() },
            MondrianError::EffectGraphEvaluationFailed {
                reason: "native detail / {file}".to_owned(),
            },
            MondrianError::AssetNotFound { asset_id: "native detail / {file}".to_owned() },
            MondrianError::AssetFileMissing { path: "native detail / {file}".to_owned() },
            MondrianError::AssetDbError { reason: "native detail / {file}".to_owned() },
            MondrianError::AiProviderFailed {
                provider: "native detail / {file}".to_owned(),
                reason: "native detail / {file}".to_owned(),
            },
            MondrianError::AiRateLimited { provider: "native detail / {file}".to_owned() },
            MondrianError::WorkflowStepFailed {
                step_id: "native detail / {file}".to_owned(),
                reason: "native detail / {file}".to_owned(),
            },
            MondrianError::WorkflowParseFailed { reason: "native detail / {file}".to_owned() },
            MondrianError::ExportFailed { reason: "native detail / {file}".to_owned() },
            MondrianError::HwEncoderUnavailable { encoder: "native detail / {file}".to_owned() },
            MondrianError::MediaSourceRevisionUnavailable {
                path: "E:/素材/{file}".to_owned(),
                reason: "native detail".to_owned(),
            },
            MondrianError::MediaSourceRevisionChanged {
                path: "E:/素材/{file}".to_owned(),
                expected: Box::default(),
                actual: Box::default(),
            },
            MondrianError::MediaExecutionResourceUnavailable {
                operation: "decode",
                source: std::io::Error::other("native detail"),
                cleanup: None,
            },
            MondrianError::MediaExecutionResourceUnavailable {
                operation: "decode",
                source: std::io::Error::other("native detail"),
                cleanup: Some("cleanup detail".to_owned()),
            },
            MondrianError::DecodeTimeout {
                asset_id: "asset".to_owned(),
                access_mode: "preview".to_owned(),
                budget_ms: u64::MAX,
                frame: u64::MAX,
                secs: 1.25,
            },
            MondrianError::DecodeBudgetExhausted {
                asset_id: "asset".to_owned(),
                access_mode: "seek".to_owned(),
                decoded_frames: u64::MAX,
                budget_frames: u64::MAX,
                target_pts: i64::MIN,
            },
            MondrianError::DecodeTemporalMismatch {
                asset_id: "asset".to_owned(),
                access_mode: "seek".to_owned(),
                requested_pts: Some(i64::MIN),
                selected_pts: None,
                selected_duration_pts: Some(i64::MAX),
            },
            MondrianError::InvalidTimelineTime(
                mondrian_core::TimelineTime::new(1, 0).expect_err("invalid denominator"),
            ),
            MondrianError::Io(std::io::Error::other("native detail")),
            MondrianError::Serialization(
                serde_json::from_str::<serde_json::Value>("{").expect_err("invalid JSON"),
            ),
            MondrianError::Cancelled,
            MondrianError::Other(anyhow::anyhow!("native detail")),
        ];
        for failure in &failures {
            let message = AppMessage::from_error(failure);
            for locale in [AppUiLocale::ZhCn, AppUiLocale::EnUs, AppUiLocale::Pseudo] {
                let localizer = Localizer::new(locale).expect("catalog");
                assert!(
                    localizer.requested.get_message(message.id).is_some(),
                    "missing {}",
                    message.id
                );
                let text = localizer.message(&message);
                assert_ne!(text, message.id, "unresolved {failure:?}");
                if locale == AppUiLocale::EnUs {
                    assert!(text.chars().next().is_some_and(|c| c.is_ascii()), "{text}");
                }
                if matches!(failure, MondrianError::DecodeTimeout { .. }) {
                    assert!(text.contains(&u64::MAX.to_string()));
                    assert!(text.contains("1.250"));
                }
                if matches!(failure, MondrianError::DecodeTemporalMismatch { .. }) {
                    assert!(text.contains(&i64::MIN.to_string()));
                    assert!(text.contains(&i64::MAX.to_string()));
                    if locale == AppUiLocale::EnUs {
                        assert!(text.contains("not supplied"));
                    }
                }
            }
        }
    }

    #[test]
    fn contextual_core_failure_retains_context_and_typed_source_in_each_locale() {
        use crate::app::{AppMessage, AppMessageValue};
        use mondrian_core::MondrianError;
        let source = MondrianError::AssetFileMissing { path: "E:/素材/{file}.mov".to_owned() };
        let error = anyhow::Error::new(source).context("mount metadata").context("project open");
        let message = AppMessage::from_anyhow(&error);
        assert_eq!(message.id, "error-context");
        assert_eq!(
            message.args.get("context"),
            Some(&AppMessageValue::Text("project open".to_owned()))
        );
        for locale in [AppUiLocale::ZhCn, AppUiLocale::EnUs, AppUiLocale::Pseudo] {
            let localizer = Localizer::new(locale).expect("catalog");
            let text = localizer.message(&message);
            assert!(text.contains("project open"));
            assert!(text.contains("mount metadata"));
            assert!(text.contains("E:/素材/{file}.mov"));
            if locale == AppUiLocale::EnUs {
                assert!(text.contains("Asset file moved or deleted"));
            }
        }
        let opaque = anyhow::anyhow!("decoder detail").context("container detail");
        let english = Localizer::new(AppUiLocale::EnUs).expect("English");
        let text = english.message(&AppMessage::from_anyhow(&opaque));
        assert!(text.contains("container detail: decoder detail"));
        let wrapped = MondrianError::Other(error);
        assert_eq!(AppMessage::from_error(&wrapped), message);
    }

    #[test]
    fn source_revision_evidence_distinguishes_failures_with_the_same_path() {
        use crate::app::AppMessage;
        use mondrian_core::{MediaFileFingerprint, MondrianError};
        let first = MondrianError::MediaSourceRevisionChanged {
            path: "source.mov".to_owned(),
            expected: Box::default(),
            actual: Box::default(),
        };
        let changed = MondrianError::MediaSourceRevisionChanged {
            path: "source.mov".to_owned(),
            expected: Box::default(),
            actual: Box::new(MediaFileFingerprint { len: Some(20), ..Default::default() }),
        };
        assert_ne!(
            AppMessage::from_error(&first),
            AppMessage::from_error(&changed)
        );
        let english = Localizer::new(AppUiLocale::EnUs).expect("English");
        assert_eq!(
            english.message(&AppMessage::from_error(&first)),
            english.message(&AppMessage::from_error(&changed))
        );
    }

    #[test]
    fn import_summary_variants_localize_counts_without_empty_interpolations() {
        use crate::app::AppMessage;
        for (id, proxies, failed) in [
            ("status-import-summary", 0, 0),
            ("status-import-summary-proxies", 1, 0),
            ("status-import-summary-partial", 0, 1),
            ("status-import-summary-proxies-partial", 1, 1),
        ] {
            for count in [1, 2] {
                let message = AppMessage::new(id)
                    .with_count("imported", count)
                    .with_count("proxies", proxies * count)
                    .with_count("failed", failed * count);
                for locale in [AppUiLocale::ZhCn, AppUiLocale::EnUs, AppUiLocale::Pseudo] {
                    let localizer = Localizer::new(locale).expect("catalog");
                    let text = localizer.message(&message);
                    assert!(!text.contains("status-import-"), "unresolved: {text}");
                    assert!(
                        !text.contains("\u{2068}\u{2069}"),
                        "empty interpolation: {text}"
                    );
                    if locale == AppUiLocale::EnUs {
                        if count == 1 {
                            assert!(text.contains("Imported one media file"));
                        } else {
                            assert!(text.contains("media files") && text.contains('2'));
                        }
                        assert_eq!(text.contains("generating"), proxies > 0);
                        assert_eq!(text.contains("failed"), failed > 0);
                        if proxies > 0 {
                            assert!(text.contains(if count == 1 {
                                "one proxy"
                            } else {
                                "proxies"
                            }));
                        }
                        if failed > 0 && count == 1 {
                            assert!(text.contains("one failed"));
                        }
                    }
                }
            }
        }
        let english = Localizer::new(AppUiLocale::EnUs).expect("English");
        assert_eq!(
            english.message(&AppMessage::new("status-import-summary").with_count("imported", 1)),
            "Imported one media file"
        );
    }

    #[test]
    fn product_message_resolves_nested_copy_on_each_locale_projection() {
        use crate::app::AppMessage;
        let nested = AppMessage::new("notification-import-complete").with_number("count", 1);
        let message = AppMessage::new("export-status-error").with_message("error", nested.clone());
        let original = message.clone();
        for locale in [AppUiLocale::ZhCn, AppUiLocale::EnUs, AppUiLocale::Pseudo] {
            let localizer = Localizer::new(locale).expect("catalog");
            let inner = localizer.message(&nested);
            let outer = localizer.message(&message);
            assert_eq!(
                outer,
                localizer.format_text("export-status-error", "error", &inner)
            );
            assert!(outer.contains(&inner), "{outer}");
        }
        let english = Localizer::new(AppUiLocale::EnUs).expect("English");
        // Fluent isolates interpolated text for correct bidirectional rendering.
        assert_eq!(
            english.message(&message),
            "Error: \u{2068}Imported one media file\u{2069}"
        );
        assert_eq!(message, original);
    }

    #[test]
    fn product_message_preserves_large_integers_and_opaque_details() {
        use crate::app::AppMessage;
        let detail = "E:/项目/Mixed Case/{clip}.mdp";
        for locale in [AppUiLocale::ZhCn, AppUiLocale::EnUs, AppUiLocale::Pseudo] {
            let localizer = Localizer::new(locale).expect("catalog");
            for value in [i64::MIN, i64::MAX, (1_i64 << 53) + 1] {
                let message = AppMessage::new("export-status-error").with_number("error", value);
                assert!(localizer.message(&message).contains(&value.to_string()));
            }
            let largest_count =
                AppMessage::new("export-status-error").with_count("error", usize::MAX);
            assert!(localizer.message(&largest_count).contains(&usize::MAX.to_string()));
            let message = AppMessage::new("export-status-error").with_text("error", detail);
            assert!(localizer.message(&message).contains(detail));
        }
    }

    #[test]
    fn pseudo_locale_preserves_opaque_arguments_and_nested_status_text() {
        let pseudo = Localizer::new(AppUiLocale::Pseudo).expect("pseudo");
        let status = pseudo.text("status-ready");
        let accessible = pseudo.format_text("status-history-accessibility", "status", &status);
        assert!(accessible.contains(&status), "{accessible}");
        let detail = "E:/项目/Mixed Case/{clip}.mdp · 18446744073709551615";
        let formatted = pseudo.format_text("export-status-error", "error", detail);
        assert!(formatted.contains(detail), "{formatted}");
    }

    #[test]
    fn pseudo_catalog_preserves_selectors_references_and_attributes() {
        let catalog = "-shared = Ready\nnotice = { $count ->\n    [1] Single { $name } { -shared }\n   *[other] Many { $name } { -shared }\n    }\n    .tip = Hint { $name }\n";
        let source = pseudo_catalog(catalog).expect("expand Fluent AST");
        let resource = FluentResource::try_new(source).expect("valid transformed resource");
        let mut bundle = FluentBundle::new(vec!["en-US".parse().expect("locale")]);
        bundle.add_resource(Arc::new(resource)).expect("resource");
        let name = "Untouched File.mdp";
        for (count, literal) in [(1, "S·i·n·g·l·e·"), (2, "M·a·n·y·")] {
            let mut args = FluentArgs::new();
            args.set("count", count);
            args.set("name", name);
            let result = format_from(&bundle, "notice", Some(&args)).expect("selector resolves");
            assert!(result.contains(literal), "{result}");
            assert!(result.contains(name), "{result}");
            assert!(result.contains("R·e·a·d·y·"), "term reference resolves");
            let attribute = bundle
                .get_message("notice")
                .expect("message")
                .get_attribute("tip")
                .expect("attribute");
            let mut errors = Vec::new();
            let tip = bundle.format_pattern(attribute.value(), Some(&args), &mut errors);
            assert!(errors.is_empty(), "{errors:?}");
            assert!(tip.contains(name));
            assert!(tip.contains("H·i·n·t·"));
        }
    }

    #[test]
    fn pseudo_locale_marks_and_expands_boundaries() {
        let pseudo = Localizer::new(AppUiLocale::Pseudo).expect("pseudo catalog");
        let text = pseudo.text("preferences-language");
        assert!(text.starts_with('⟦') && text.ends_with('⟧'));
        assert!(
            text.len()
                > Localizer::new(AppUiLocale::ZhCn)
                    .expect("Chinese catalog")
                    .text("preferences-language")
                    .len()
        );
    }
}
