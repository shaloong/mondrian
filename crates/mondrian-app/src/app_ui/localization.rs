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
    /// A preference written by a newer release falls back safely.
    #[serde(other)]
    Unknown,
}

impl AppUiLocalePreference {
    /// Resolve the one UI locale from a captured system BCP 47 tag.
    pub fn resolve(self, system_tag: Option<&str>) -> AppUiLocale {
        match self {
            Self::ZhCn => AppUiLocale::ZhCn,
            Self::EnUs => AppUiLocale::EnUs,
            Self::Pseudo => AppUiLocale::Pseudo,
            Self::System | Self::Unknown => match system_tag {
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
        let value = format_from(&self.requested, message_id, args)
            .or_else(|| format_from(&self.fallback, message_id, args))
            .unwrap_or_else(|| message_id.to_owned());
        if self.locale == AppUiLocale::Pseudo {
            pseudo_expand(&value)
        } else {
            value
        }
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

fn build_bundle(
    locale: AppUiLocale,
) -> Result<FluentBundle<Arc<FluentResource>>, LocalizationError> {
    let tag = locale.language_tag();
    let langid: LanguageIdentifier =
        tag.parse().map_err(|_| LocalizationError::InvalidLocale(tag.to_owned()))?;
    let cached = match locale {
        AppUiLocale::ZhCn | AppUiLocale::Pseudo => &ZH_CN_RESOURCE,
        AppUiLocale::EnUs => &EN_US_RESOURCE,
    };
    let resource = cached
        .get_or_init(|| {
            FluentResource::try_new(locale.catalog().to_owned())
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

fn pseudo_expand(text: &str) -> String {
    let mut expanded = String::with_capacity(text.len().saturating_mul(2).saturating_add(6));
    expanded.push('⟦');
    for character in text.chars() {
        expanded.push(character);
        if character.is_alphabetic() {
            expanded.push('·');
        }
    }
    expanded.push('⟧');
    expanded
}

#[cfg(test)]
mod tests {
    use super::*;

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
