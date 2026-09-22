//! Shared interface dictionaries. English is the default; language selection is explicit.
use std::{collections::HashMap, sync::LazyLock};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    English,
    Chinese,
    TraditionalChinese,
    Korean,
    German,
    Spanish,
    French,
    Danish,
    Japanese,
    Polish,
    Russian,
    Arabic,
    Norwegian,
    Portuguese,
    Thai,
    Bosnian,
    Turkish,
}

impl Language {
    pub const ALL: [(Self, &str, &str); 17] = [
        (Self::English, "en", "English"),
        (Self::Chinese, "zh-CN", "简体中文"),
        (Self::TraditionalChinese, "zh-TW", "繁體中文"),
        (Self::Korean, "ko", "한국어"),
        (Self::German, "de", "Deutsch"),
        (Self::Spanish, "es", "Español"),
        (Self::French, "fr", "Français"),
        (Self::Danish, "da", "Dansk"),
        (Self::Japanese, "ja", "日本語"),
        (Self::Polish, "pl", "Polski"),
        (Self::Russian, "ru", "Русский"),
        (Self::Arabic, "ar", "العربية"),
        (Self::Norwegian, "nb", "Norsk"),
        (Self::Portuguese, "pt-BR", "Português (Brasil)"),
        (Self::Thai, "th", "ไทย"),
        (Self::Bosnian, "bs", "Bosanski"),
        (Self::Turkish, "tr", "Türkçe"),
    ];

    pub fn parse(value: &str) -> Option<Self> {
        let value = value
            .trim()
            .split('.')
            .next()?
            .replace('_', "-")
            .to_ascii_lowercase();
        if value == "zht"
            || value.starts_with("zh-hant")
            || value.starts_with("zh-tw")
            || value.starts_with("zh-hk")
            || value.starts_with("zh-mo")
        {
            return Some(Self::TraditionalChinese);
        }
        match value.split('-').next().unwrap_or_default() {
            "en" => Some(Self::English),
            "zh" => Some(Self::Chinese),
            "ko" => Some(Self::Korean),
            "de" => Some(Self::German),
            "es" => Some(Self::Spanish),
            "fr" => Some(Self::French),
            "da" => Some(Self::Danish),
            "ja" => Some(Self::Japanese),
            "pl" => Some(Self::Polish),
            "ru" => Some(Self::Russian),
            "ar" => Some(Self::Arabic),
            "nb" | "no" | "nn" => Some(Self::Norwegian),
            "pt" | "br" => Some(Self::Portuguese),
            "th" => Some(Self::Thai),
            "bs" => Some(Self::Bosnian),
            "tr" => Some(Self::Turkish),
            _ => None,
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Chinese => "zh-CN",
            Self::TraditionalChinese => "zh-TW",
            Self::Korean => "ko",
            Self::German => "de",
            Self::Spanish => "es",
            Self::French => "fr",
            Self::Danish => "da",
            Self::Japanese => "ja",
            Self::Polish => "pl",
            Self::Russian => "ru",
            Self::Arabic => "ar",
            Self::Norwegian => "nb",
            Self::Portuguese => "pt-BR",
            Self::Thai => "th",
            Self::Bosnian => "bs",
            Self::Turkish => "tr",
        }
    }

    pub fn text(self, key: &str) -> &str {
        DICTIONARIES
            .get(self.code())
            .and_then(|dict| dict.get(key))
            .or_else(|| DICTIONARIES["en"].get(key))
            .map_or(key, String::as_str)
    }
}

static DICTIONARIES: LazyLock<HashMap<&'static str, HashMap<String, String>>> =
    LazyLock::new(|| {
        [
            ("en", include_str!("../../../locales/en.json")),
            ("zh-CN", include_str!("../../../locales/zh-CN.json")),
            ("zh-TW", include_str!("../../../locales/zh-TW.json")),
            ("ko", include_str!("../../../locales/ko.json")),
            ("de", include_str!("../../../locales/de.json")),
            ("es", include_str!("../../../locales/es.json")),
            ("fr", include_str!("../../../locales/fr.json")),
            ("da", include_str!("../../../locales/da.json")),
            ("ja", include_str!("../../../locales/ja.json")),
            ("pl", include_str!("../../../locales/pl.json")),
            ("ru", include_str!("../../../locales/ru.json")),
            ("ar", include_str!("../../../locales/ar.json")),
            ("nb", include_str!("../../../locales/nb.json")),
            ("pt-BR", include_str!("../../../locales/pt-BR.json")),
            ("th", include_str!("../../../locales/th.json")),
            ("bs", include_str!("../../../locales/bs.json")),
            ("tr", include_str!("../../../locales/tr.json")),
        ]
        .into_iter()
        .map(|(code, json)| {
            (
                code,
                serde_json::from_str(json).expect("valid bundled locale"),
            )
        })
        .collect()
    });

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_region_and_underscore_locales() {
        for (input, expected) in [
            ("ja_JP.UTF-8", Language::Japanese),
            ("zh-CN", Language::Chinese),
            ("zh_Hant_TW", Language::TraditionalChinese),
            ("zh-HK", Language::TraditionalChinese),
            ("pt_PT.UTF-8", Language::Portuguese),
            ("nn-NO", Language::Norwegian),
        ] {
            assert_eq!(Language::parse(input), Some(expected));
        }
        assert_eq!(Language::parse("xx"), None);
        assert_eq!(Language::default(), Language::English);
    }
    #[test]
    fn shared_catalogs_have_complete_keys_and_matching_language_inventory() {
        let inventory: serde_json::Value =
            serde_json::from_str(include_str!("../../../locales/languages.json")).unwrap();
        assert_eq!(inventory.as_array().unwrap().len(), Language::ALL.len());
        for (language, code, label) in Language::ALL {
            assert_eq!(Language::parse(code), Some(language));
            assert_eq!(language.code(), code);
            assert!(
                inventory
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["code"] == code && entry["label"] == label)
            );
            let dict = &DICTIONARIES[code];
            assert_eq!(dict.len(), DICTIONARIES["en"].len(), "{code}");
            for key in DICTIONARIES["en"].keys() {
                assert!(
                    !dict.get(key).is_none_or(|text| text.trim().is_empty()),
                    "{code}: {key}"
                );
            }
            assert_eq!(
                language.text("untranslated diagnostic"),
                "untranslated diagnostic"
            );
        }
    }
}
