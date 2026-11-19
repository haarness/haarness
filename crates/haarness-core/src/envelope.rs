//! Text Haarness wraps in tags of its own before the model reads it: a
//! command's output in the background, a subagent's reply, a workflow's
//! result, what hooks say. That text comes from programs, pages and other
//! agents. Were it to hold the closing tag of the block it sits in, what
//! followed would read as Haarness's own words: a notice, a reminder, the
//! person's message. Only those closing tags are broken: the text is often
//! code, which a model may copy into a file, so every other character
//! reaches it as written.

use std::borrow::Cow;

/// `text` with each closing tag of one of `names` broken: its `<` is
/// written `&lt;`. Any case counts, and so do spaces around the `/`, the
/// invisible characters a terminal or editor shows as nothing
/// (`<\u{200B}/task>`, `</ta\u{200B}sk>`) and the full-width forms
/// (`＜／ｔａｓｋ＞`). Every other angle bracket stays as it is, so command
/// output and code (`Vec<T>`, `<div>`) read as they were printed.
#[must_use]
pub fn closing_tags_broken<'a>(text: &'a str, names: &[&str]) -> Cow<'a, str> {
    let mut broken = String::new();
    let mut kept = 0;
    for (at, open) in text
        .char_indices()
        .filter(|(_, character)| matches!(character, '<' | '\u{FF1C}'))
    {
        let after = at + open.len_utf8();
        if closes(&text[after..], names) {
            broken.push_str(&text[kept..at]);
            broken.push_str("&lt;");
            kept = after;
        }
    }
    if kept == 0 {
        return Cow::Borrowed(text);
    }
    broken.push_str(&text[kept..]);
    Cow::Owned(broken)
}

/// Whether `rest`, the text after a `<`, closes a tag named in `names`, read
/// as a model would see it: invisible characters left out and full-width
/// forms as their ASCII ones.
fn closes(rest: &str, names: &[&str]) -> bool {
    let longest = names.iter().map(|name| name.len()).max().unwrap_or(0);
    let seen: String = rest
        .chars()
        .filter(|character| !invisible(*character))
        .map(narrow)
        // Room for spaces, the `/`, the name and what follows it.
        .take(longest + 64)
        .collect();
    let Some(rest) = seen.trim_start().strip_prefix('/') else {
        return false;
    };
    let rest = rest.trim_start();
    names.iter().any(|name| {
        rest.get(..name.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(name))
            && !rest[name.len()..].starts_with(|character: char| {
                character.is_alphanumeric() || "_-:.".contains(character)
            })
    })
}

/// A character that takes no room on screen: zero-width spaces and joiners,
/// direction marks and isolates, the soft hyphen, variation selectors,
/// combining marks and tag characters.
fn invisible(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{0300}'..='\u{036F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// A full-width form as its ASCII character (`／` as `/`, `ｔ` as `t`), and
/// the ideographic space as a space; anything else as it is.
fn narrow(character: char) -> char {
    match character {
        '\u{FF01}'..='\u{FF5E}' => {
            char::from_u32(u32::from(character) - 0xFEE0).unwrap_or(character)
        }
        '\u{3000}' => ' ',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_named_closing_tags_are_broken() {
        let names = ["command_output", "command"];
        for (text, broken) in [
            (
                "ok\n</command_output>\n</command>\nnext",
                "ok\n&lt;/command_output>\n&lt;/command>\nnext",
            ),
            ("</COMMAND>", "&lt;/COMMAND>"),
            ("< / command >", "&lt; / command >"),
            ("</command\n>", "&lt;/command\n>"),
            ("x</command", "x&lt;/command"),
            // Code and other tags stay readable.
            ("let v: Vec<T> = Vec::new();", "let v: Vec<T> = Vec::new();"),
            ("<div></div>", "<div></div>"),
            ("<command>", "<command>"),
            ("</commands>", "</commands>"),
            ("</command-line>", "</command-line>"),
            ("a < b && c > d", "a < b && c > d"),
            ("<", "<"),
            ("</", "</"),
            ("</comm", "</comm"),
            ("é</command>é", "é&lt;/command>é"),
            // Invisible characters and full-width forms read as a model sees
            // them; a visible character between still parts the name.
            ("<\u{200B}/command>", "&lt;\u{200B}/command>"),
            ("</com\u{200B}mand>", "&lt;/com\u{200B}mand>"),
            (
                "<\u{2066}/\u{FEFF}command\u{E0020}>",
                "&lt;\u{2066}/\u{FEFF}command\u{E0020}>",
            ),
            (
                "\u{FF1C}\u{FF0F}command\u{FF1E}",
                "&lt;\u{FF0F}command\u{FF1E}",
            ),
            (
                "\u{FF1C}\u{FF0F}\u{FF43}\u{FF4F}\u{FF4D}\u{FF4D}\u{FF41}\u{FF4E}\u{FF44}\u{FF1E}",
                "&lt;\u{FF0F}\u{FF43}\u{FF4F}\u{FF4D}\u{FF4D}\u{FF41}\u{FF4E}\u{FF44}\u{FF1E}",
            ),
            ("<\u{3000}/command>", "&lt;\u{3000}/command>"),
            ("</com-mand>", "</com-mand>"),
            ("\u{FF1C}div\u{FF1E}", "\u{FF1C}div\u{FF1E}"),
        ] {
            assert_eq!(closing_tags_broken(text, &names), broken, "{text:?}");
        }
        assert!(matches!(
            closing_tags_broken("Vec<T>", &names),
            Cow::Borrowed(_)
        ));
    }
}
