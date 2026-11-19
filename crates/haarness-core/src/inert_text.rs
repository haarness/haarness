//! Data written into text a model reads as Haarness's own (a reviewer's
//! prompt, a note in a chat's history), made unable to read as part of
//! that text: no line break, tag or separator in it starts or ends anything.

/// `text` as a JSON string on one line, its angle brackets escaped too, so
/// nothing in it reads as a line or a tag of the prompt around it.
pub(crate) fn quoted(text: &str) -> String {
    inert(&serde_json::to_string(text).unwrap_or_default())
}

/// `text` with its angle brackets escaped as JSON escapes, so a `</action>`
/// or `<transcript>` inside data never closes or opens a section, and with
/// the line and paragraph separators JSON leaves as they are (a model may
/// read them as a new line) escaped too.
pub(crate) fn inert(text: &str) -> String {
    let mut inert = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => inert.push_str("\\u003c"),
            '>' => inert.push_str("\\u003e"),
            '\u{2028}' => inert.push_str("\\u2028"),
            '\u{2029}' => inert.push_str("\\u2029"),
            '\u{85}' => inert.push_str("\\u0085"),
            c => inert.push(c),
        }
    }
    inert
}
