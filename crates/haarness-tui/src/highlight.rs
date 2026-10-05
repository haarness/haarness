//! Minimal hand-rolled syntax highlighting: five roles, common languages,
//! zero dependencies. A quiet terminal needs hints, not a grammar engine;
//! unknown languages fall back to plain text.

use haarness_opentui::Span;

use crate::theme::{SyntaxTheme, Theme};

struct LanguageSpec {
    names: &'static [&'static str],
    keywords: &'static [&'static str],
    line_comment: &'static str,
}

const LANGUAGES: &[LanguageSpec] = &[
    LanguageSpec {
        names: &["rust", "rs"],
        keywords: &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
            "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "type",
            "unsafe", "use", "where", "while",
        ],
        line_comment: "//",
    },
    LanguageSpec {
        names: &["javascript", "js", "typescript", "ts", "tsx", "jsx"],
        keywords: &[
            "async",
            "await",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "default",
            "delete",
            "else",
            "export",
            "extends",
            "finally",
            "for",
            "function",
            "if",
            "import",
            "in",
            "instanceof",
            "interface",
            "let",
            "new",
            "of",
            "return",
            "static",
            "switch",
            "throw",
            "try",
            "type",
            "typeof",
            "var",
            "while",
            "yield",
        ],
        line_comment: "//",
    },
    LanguageSpec {
        names: &["python", "py"],
        keywords: &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
            "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in",
            "is", "lambda", "not", "or", "pass", "raise", "return", "try", "while", "with",
            "yield",
        ],
        line_comment: "#",
    },
    LanguageSpec {
        names: &["go"],
        keywords: &[
            "break",
            "case",
            "chan",
            "const",
            "continue",
            "default",
            "defer",
            "else",
            "for",
            "func",
            "go",
            "goto",
            "if",
            "import",
            "interface",
            "map",
            "package",
            "range",
            "return",
            "select",
            "struct",
            "switch",
            "type",
            "var",
        ],
        line_comment: "//",
    },
    LanguageSpec {
        names: &["sh", "bash", "shell", "zsh"],
        keywords: &[
            "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in",
            "local", "return", "then", "until", "while",
        ],
        line_comment: "#",
    },
    LanguageSpec {
        names: &["toml", "ini"],
        keywords: &["true", "false"],
        line_comment: "#",
    },
    LanguageSpec {
        names: &["json"],
        keywords: &["true", "false", "null"],
        line_comment: "\u{0}", // JSON has no comments
    },
];

fn language(name: &str) -> Option<&'static LanguageSpec> {
    let name = name.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|spec| spec.names.contains(&name.as_str()))
}

/// Highlight one source line into spans. Every span carries the code-block
/// surface background supplied by the caller through `base`.
pub fn highlight_line(line: &str, language_name: &str, theme: &Theme) -> Vec<Span> {
    let syntax = &theme.syntax;
    let Some(spec) = language(language_name) else {
        return vec![Span::new(line, syntax.function).on(theme.surface)];
    };
    let mut spans: Vec<Span> = Vec::new();
    let push = |spans: &mut Vec<Span>, text: &str, color| {
        if !text.is_empty() {
            spans.push(Span::new(text, color).on(theme.surface));
        }
    };

    let mut rest = line;
    let mut plain = String::new();
    while !rest.is_empty() {
        // Line comments swallow everything after them.
        if !spec.line_comment.is_empty()
            && spec.line_comment != "\u{0}"
            && rest.starts_with(spec.line_comment)
        {
            push(&mut spans, &plain, syntax.function);
            plain = String::new();
            push(&mut spans, rest, syntax.comment);
            return finish(spans, plain, syntax, theme);
        }
        let mut characters = rest.chars();
        let character = characters.next().unwrap_or_default();
        if character == '"' || character == '\'' || character == '`' {
            // String literal to the matching unescaped quote (or line end).
            push(&mut spans, &plain, syntax.function);
            plain = String::new();
            let mut length = character.len_utf8();
            let mut escaped = false;
            for inner in rest[length..].chars() {
                length += inner.len_utf8();
                if escaped {
                    escaped = false;
                } else if inner == '\\' {
                    escaped = true;
                } else if inner == character {
                    break;
                }
            }
            push(&mut spans, &rest[..length], syntax.string);
            rest = &rest[length..];
            continue;
        }
        if character.is_ascii_digit() && !plain.ends_with(|c: char| c.is_alphanumeric() || c == '_')
        {
            let length = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_'))
                .unwrap_or(rest.len());
            push(&mut spans, &plain, syntax.function);
            plain = String::new();
            push(&mut spans, &rest[..length], syntax.constant);
            rest = &rest[length..];
            continue;
        }
        if character.is_alphabetic() || character == '_' {
            let length = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let word = &rest[..length];
            let after = rest[length..].trim_start();
            push(&mut spans, &plain, syntax.function);
            plain = String::new();
            if spec.keywords.contains(&word) {
                push(&mut spans, word, syntax.keyword);
            } else if after.starts_with('(') {
                push(&mut spans, word, syntax.constant);
            } else {
                push(&mut spans, word, syntax.function);
            }
            rest = &rest[length..];
            continue;
        }
        plain.push(character);
        rest = &rest[character.len_utf8()..];
    }
    finish(spans, plain, syntax, theme)
}

fn finish(mut spans: Vec<Span>, plain: String, syntax: &SyntaxTheme, theme: &Theme) -> Vec<Span> {
    if !plain.is_empty() {
        spans.push(Span::new(plain, syntax.function).on(theme.surface));
    }
    if spans.is_empty() {
        spans.push(Span::new("", syntax.function).on(theme.surface));
    }
    spans
}

// ---------------------------------------------------------------------------
// Syntect backend: real grammars for ~50 languages, colored through the
// active theme's five syntax roles so user themes keep working.
// ---------------------------------------------------------------------------

fn syntax_set() -> &'static syntect::parsing::SyntaxSet {
    use std::sync::OnceLock;
    static SET: OnceLock<syntect::parsing::SyntaxSet> = OnceLock::new();
    SET.get_or_init(syntect::parsing::SyntaxSet::load_defaults_newlines)
}

/// Build a syntect theme whose scope colors are the five haarness roles.
fn role_theme(theme: &Theme) -> syntect::highlighting::Theme {
    use syntect::highlighting::{
        ScopeSelectors, StyleModifier, Theme as SyntectTheme, ThemeItem, ThemeSettings,
    };
    fn color(value: haarness_opentui::Color) -> syntect::highlighting::Color {
        let [r, g, b, _] = value.components();
        #[allow(clippy::cast_possible_truncation)]
        syntect::highlighting::Color {
            r: r.min(255) as u8,
            g: g.min(255) as u8,
            b: b.min(255) as u8,
            a: 255,
        }
    }
    let roles = &theme.syntax;
    let settings = ThemeSettings {
        foreground: Some(color(roles.function)),
        ..ThemeSettings::default()
    };
    let mut items = Vec::new();
    for (selector, role) in [
        ("comment, punctuation.definition.comment", roles.comment),
        (
            "string, string.quoted, punctuation.definition.string",
            roles.string,
        ),
        (
            "constant, constant.numeric, constant.language, constant.character, support.constant",
            roles.constant,
        ),
        (
            "entity.name.type, support.type, support.class, entity.name.class",
            roles.constant,
        ),
        (
            "keyword, keyword.control, keyword.operator.word, storage, storage.type, \
             storage.modifier, variable.language",
            roles.keyword,
        ),
        (
            "entity.name.function, support.function, variable.function, meta.function-call",
            roles.function,
        ),
    ] {
        if let Ok(scope) = selector.parse::<ScopeSelectors>() {
            items.push(ThemeItem {
                scope,
                style: StyleModifier {
                    foreground: Some(color(role)),
                    background: None,
                    font_style: None,
                },
            });
        }
    }
    SyntectTheme {
        name: None,
        author: None,
        settings,
        scopes: items,
    }
}

/// Highlight a whole code block with multi-line-correct grammar state.
/// Unknown languages fall back to the legacy single-line highlighter.
pub fn highlight_block(code: &str, language_name: &str, theme: &Theme) -> Vec<Vec<Span>> {
    let set = syntax_set();
    let token = language_name.trim();
    let syntax =
        set.find_syntax_by_token(token)
            .or_else(|| match token.to_ascii_lowercase().as_str() {
                "shell" | "zsh" => set.find_syntax_by_token("bash"),
                "jsonc" => set.find_syntax_by_token("json"),
                "ts" | "tsx" => set.find_syntax_by_token("typescript"),
                _ => None,
            });
    let Some(syntax) = syntax else {
        return code
            .lines()
            .map(|row| highlight_line(row, language_name, theme))
            .collect();
    };
    let syntect_theme = role_theme(theme);
    let mut highlighter = syntect::easy::HighlightLines::new(syntax, &syntect_theme);
    code.lines()
        .map(|row| {
            let line = format!("{row}\n");
            match highlighter.highlight_line(&line, set) {
                Ok(runs) => {
                    let spans: Vec<Span> = runs
                        .into_iter()
                        .map(|(style, text)| {
                            let text = text.trim_end_matches('\n');
                            Span::new(
                                text,
                                haarness_opentui::Color::rgb(
                                    style.foreground.r,
                                    style.foreground.g,
                                    style.foreground.b,
                                ),
                            )
                            .on(theme.surface)
                        })
                        .filter(|span| !span.text.is_empty())
                        .collect();
                    if spans.is_empty() {
                        vec![Span::new("", theme.syntax.function).on(theme.surface)]
                    } else {
                        spans
                    }
                }
                Err(_) => highlight_line(row, language_name, theme),
            }
        })
        .collect()
}

/// Highlight a block and leave the background to the caller. The review
/// viewer paints its own added/removed fill behind every row, so a baked-in
/// surface colour would show through as a second, wrong band.
pub fn highlight_block_transparent(
    code: &str,
    language_name: &str,
    theme: &Theme,
) -> Vec<Vec<Span>> {
    let mut lines = highlight_block(code, language_name, theme);
    for line in &mut lines {
        for span in line.iter_mut() {
            span.bg = None;
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::highlight_line;
    use crate::theme::Theme;

    #[test]
    fn rust_keywords_strings_comments_and_numbers_get_roles() {
        let theme = Theme::editorial_dark();
        let spans = highlight_line("let x = \"hi\"; // note 42", "rust", &theme);
        let find = |text: &str| {
            spans
                .iter()
                .find(|span| span.text.contains(text))
                .unwrap_or_else(|| panic!("missing {text:?} in {spans:?}"))
        };
        assert_eq!(find("let").fg, theme.syntax.keyword);
        assert_eq!(find("\"hi\"").fg, theme.syntax.string);
        assert_eq!(find("// note 42").fg, theme.syntax.comment);
        let spans = highlight_line("count(3)", "rust", &theme);
        assert_eq!(find_in(&spans, "count").fg, theme.syntax.constant);
        assert_eq!(find_in(&spans, "3").fg, theme.syntax.constant);
    }

    fn find_in<'a>(spans: &'a [haarness_opentui::Span], text: &str) -> &'a haarness_opentui::Span {
        spans
            .iter()
            .find(|span| span.text.contains(text))
            .unwrap_or_else(|| panic!("missing {text:?} in {spans:?}"))
    }

    #[test]
    fn unknown_languages_render_plain_on_surface() {
        let theme = Theme::editorial_dark();
        let spans = highlight_line("whatever ??", "brainfuck", &theme);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].bg, Some(theme.surface));
    }
}
