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

    /// [`Self::text`] with its `{placeholder}`s filled in, in one pass: a
    /// value is inserted as it is, so a file or provider name holding
    /// `{count}` is never filled in again.
    pub fn format(self, key: &str, values: &[(&str, &str)]) -> String {
        let text = self.text(key);
        let mut filled = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            filled.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            let value = after.split_once('}').and_then(|(name, _)| {
                values
                    .iter()
                    .find(|(known, _)| *known == name)
                    .map(|(_, value)| (name.len(), *value))
            });
            if let Some((length, value)) = value {
                filled.push_str(value);
                rest = &after[length + 1..];
            } else {
                filled.push('{');
                rest = after;
            }
        }
        filled.push_str(rest);
        filled
    }

    /// Languages written right to left. The terminal grid itself stays left
    /// to right; a bidi-aware terminal reorders the text inside each row.
    ///
    /// Haarness does not wrap hints in bidi isolates (U+2066…U+2069) or
    /// marks (U+200E): the native renderer gives each of those zero-width
    /// characters a cell of its own, while the terminal gives it none, so
    /// every mark would shift the rest of the row by a cell.
    pub const fn is_rtl(self) -> bool {
        matches!(self, Self::Arabic)
    }

    /// Translated key hints ("enter select", "esc close"), joined the way
    /// dialog footers join them.
    pub fn hints(self, keys: &[&str]) -> String {
        keys.iter()
            .map(|key| self.text(key))
            .collect::<Vec<_>>()
            .join("  \u{00b7}  ")
    }

    /// A list position ("1–7 of 61") followed by key hints, as a picker's
    /// footer reads.
    pub fn range_hints(self, from: usize, to: usize, total: usize, keys: &[&str]) -> String {
        let range = if total == 0 {
            self.text("0 of 0").to_owned()
        } else {
            self.format(
                "{from}–{to} of {total}",
                &[
                    ("from", &from.to_string()),
                    ("to", &to.to_string()),
                    ("total", &total.to_string()),
                ],
            )
        };
        if keys.is_empty() {
            range
        } else {
            format!("{range}  \u{00b7}  {}", self.hints(keys))
        }
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
    fn format_inserts_each_value_as_it_is() {
        let english = Language::English;
        assert_eq!(
            english.format("{a} then {b} and {a}", &[("a", "{b}"), ("b", "two $1 {a}")]),
            "{b} then two $1 {a} and {b}"
        );
        assert_eq!(
            english.format("{unknown} {a", &[("a", "x")]),
            "{unknown} {a"
        );
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

    /// Read the Rust string literal that opens at `start`, decoding the
    /// escapes interface text uses.
    fn literal_at(source: &str, start: usize) -> Option<(String, usize)> {
        let mut chars = source[start..].char_indices();
        if chars.next()?.1 != '"' {
            return None;
        }
        let mut value = String::new();
        while let Some((index, character)) = chars.next() {
            match character {
                '"' => return Some((value, start + index + 1)),
                '\\' => match chars.next()?.1 {
                    'n' => value.push('\n'),
                    'u' => {
                        let mut hex = String::new();
                        for (_, digit) in chars.by_ref() {
                            match digit {
                                '{' => {}
                                '}' => break,
                                digit => hex.push(digit),
                            }
                        }
                        value.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    other => value.push(other),
                },
                character => value.push(character),
            }
        }
        None
    }

    /// A key handed to `text`, `format` or a hint list as a literal shows
    /// English in every language unless the catalog has it.
    #[test]
    fn literal_interface_keys_are_in_the_catalog() {
        fn sources(directory: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    sources(&path, out);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        sources(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut missing = Vec::new();
        for path in files {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name == "locale.rs" || name == "tests.rs" || name.ends_with("_tests.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let mut keys = Vec::new();
            for call in [".text(\"", ".format(\""] {
                for (index, _) in source.match_indices(call) {
                    if let Some((key, _)) = literal_at(&source, index + call.len() - 1) {
                        keys.push(key);
                    }
                }
            }
            for call in ["hints(&[", "range(&[", "hints(\n"] {
                for (index, _) in source.match_indices(call) {
                    let end = source[index..]
                        .find(']')
                        .map_or(source.len(), |end| index + end);
                    let mut cursor = index;
                    while let Some(offset) = source[cursor..end].find('"') {
                        let Some((key, next)) = literal_at(&source, cursor + offset) else {
                            break;
                        };
                        keys.push(key);
                        cursor = next;
                    }
                }
            }
            for key in keys {
                // Date patterns are formats, not words.
                if !key.contains('%') && !DICTIONARIES["en"].contains_key(&key) {
                    missing.push(format!("{name}: {key}"));
                }
            }
        }
        assert!(missing.is_empty(), "{missing:#?}");
    }

    #[test]
    fn footers_join_translated_hints_after_the_list_position() {
        let english =
            Language::English.range_hints(1, 7, 61, &["\u{2191}\u{2193} navigate", "esc close"]);
        assert_eq!(
            english,
            "1–7 of 61  \u{b7}  \u{2191}\u{2193} navigate  \u{b7}  esc close"
        );
        assert_eq!(Language::English.range_hints(1, 0, 0, &[]), "0 of 0");
        assert_eq!(
            Language::French.hints(&["Enter to apply", "Escape to cancel"]),
            format!(
                "{}  \u{b7}  {}",
                Language::French.text("Enter to apply"),
                Language::French.text("Escape to cancel")
            )
        );
    }

    /// Why hints carry no bidi isolates: the native renderer stores a
    /// zero-width mark in a cell of its own, so the next span, placed by
    /// display width, overwrites the glyph after the mark. When this starts
    /// failing, the renderer handles zero-width marks and Arabic hint rows
    /// can be isolated (TA20).
    #[test]
    fn native_renderer_still_gives_a_bidi_mark_a_cell() {
        let mut renderer = haarness_opentui::Renderer::new(20, 2, true).unwrap();
        let white = haarness_opentui::Color::rgb(255, 255, 255);
        let drawn = renderer.snapshot_lines(&[haarness_opentui::Line::new(vec![
            haarness_opentui::Span::new("ab\u{2066}cd", white),
            haarness_opentui::Span::new("|", white),
        ])]);
        assert!(drawn.starts_with("ab\u{2066}c|"), "{drawn:?}");
    }
}
