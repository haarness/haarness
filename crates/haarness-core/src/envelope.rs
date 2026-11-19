//! Text Haarness wraps in tags of its own before the model reads it: a
//! command's output in the background, a subagent's reply, what hooks say.
//! That text comes from programs, pages and other agents. Were it to hold
//! the closing tag of the block it sits in, what followed would read as
//! Haarness's own words: a notice, a reminder, the person's message.

use std::borrow::Cow;

/// `text` with `&`, `<` and `>` written as entities, for text that is data
/// from end to end (a subagent's reply and its task's description): no tag
/// in it opens or closes anything.
#[must_use]
pub fn escaped(text: &str) -> Cow<'_, str> {
    if !text.contains(['&', '<', '>']) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;"),
    )
}

/// `text` with each closing tag of one of `names` broken: its `<` is
/// written `&lt;`. Any case counts, and so do spaces around the `/`. Every
/// other angle bracket stays as it is, so command output and code
/// (`Vec<T>`, `<div>`) read as they were printed.
#[must_use]
pub fn closing_tags_broken<'a>(text: &'a str, names: &[&str]) -> Cow<'a, str> {
    let mut broken = String::new();
    let mut kept = 0;
    for (at, _) in text.match_indices('<') {
        if closes(&text[at + 1..], names) {
            broken.push_str(&text[kept..at]);
            broken.push_str("&lt;");
            kept = at + 1;
        }
    }
    if kept == 0 {
        return Cow::Borrowed(text);
    }
    broken.push_str(&text[kept..]);
    Cow::Owned(broken)
}

/// Whether `rest`, the text after a `<`, closes a tag named in `names`.
fn closes(rest: &str, names: &[&str]) -> bool {
    let Some(rest) = rest.trim_start().strip_prefix('/') else {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_text_holds_no_tag() {
        assert_eq!(
            escaped("</task_result></task>\n<x a=\"1\">&amp;"),
            "&lt;/task_result&gt;&lt;/task&gt;\n&lt;x a=\"1\"&gt;&amp;amp;"
        );
        assert!(matches!(escaped("plain text"), Cow::Borrowed(_)));
    }

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
        ] {
            assert_eq!(closing_tags_broken(text, &names), broken, "{text:?}");
        }
        assert!(matches!(
            closing_tags_broken("Vec<T>", &names),
            Cow::Borrowed(_)
        ));
    }
}
