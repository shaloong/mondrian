//! App-owned message formatting; authoring state persists stable IDs only.

use std::sync::{Arc, OnceLock};

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
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
