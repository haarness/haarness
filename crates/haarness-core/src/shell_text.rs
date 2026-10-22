//! Values that go into the text a shell parses: a hook's command, an MCP
//! server's `headersHelper`, a command's or skill's `` !`…` `` snippet. A
//! value pasted into that text as it is becomes shell syntax: a folder named
//! `p$(touch x)` runs `touch`, and one holding a space splits a word. The
//! scanner here reads the text as the shell reads its quotes and puts each
//! placeholder where it stands either for the shell to expand from the
//! environment ([`Placeholders::Expand`]) or as its value quoted for that
//! spot ([`Placeholders::Fill`]).

use std::fmt::Write as _;

/// The shells whose script text a placeholder must not be pasted into.
/// `cmd` is not one: it expands `%NAME%` before it parses, so a value
/// holding `&` splits the command either way, and its scripts keep the value
/// filled in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellKind {
    Posix,
    PowerShell,
}

/// What becomes of a placeholder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placeholders {
    /// Left for the shell to expand from the environment the command gets.
    Expand,
    /// Replaced by its value, quoted for where it stands (POSIX shells; a
    /// command without that environment, such as the agent's shell tool).
    Fill,
}

/// `${NAME}` or `${NAME:-fallback}`, for any variable name, at the start of
/// `rest`: its length in bytes.
#[must_use]
pub fn variable_at(rest: &str) -> Option<usize> {
    let body = rest.strip_prefix("${")?;
    let token = &body[..body.find('}')?];
    let name = placeholder_name(token);
    (name.starts_with(|character: char| character.is_ascii_alphabetic() || character == '_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && (token.len() == name.len() || token[name.len()..].starts_with(":-")))
    .then_some(token.len() + 3)
}

/// The variable a placeholder names: `NAME` of `${NAME}`, `${NAME:-x}` or
/// of the bare `NAME:-x` inside the braces.
fn placeholder_name(placeholder: &str) -> &str {
    let token = placeholder
        .strip_prefix("${")
        .and_then(|token| token.strip_suffix('}'))
        .unwrap_or(placeholder);
    token.split_once(":-").map_or(token, |(name, _)| name)
}

/// `value` as one word a POSIX shell reads back as it is: bare when it holds
/// only characters no shell reads specially, else single-quoted.
#[must_use]
pub fn posix_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-./:@%+,".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Whether a value can be written inside PowerShell single quotes as it is:
/// only characters no PowerShell reads specially there.
fn powershell_plain(value: &str) -> bool {
    value.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || matches!(character, ' ' | '_' | '.' | '/' | ':' | '\\' | '-')
    })
}

/// `command` with each placeholder `find` reports (its length at the start
/// of the text) placed as the scanner reads the shell's quotes.
///
/// [`Placeholders::Expand`], from the environment:
/// - outside single quotes a POSIX shell's `${NAME}` stays and PowerShell's
///   becomes `${env:NAME}`;
/// - inside single quotes, where nothing expands, POSIX gets `'"${NAME}"'`
///   (close, expand in double quotes, reopen), which stays safe even where
///   the scanner misjudges the quotes (a comment, a here-document, `$'…'`);
///   PowerShell gets the value itself only when it is made of plain
///   characters, every single-quote character (ASCII and U+2018–U+201B)
///   doubled, and `${env:NAME}` otherwise, which is then read literally:
///   wrong, but never run.
/// - In PowerShell each statement that starts with a placeholder (at the
///   start, after `;`, `|`, `&&`, `||` or a line break) is a path to run, so
///   that word is quoted and called with `&`.
///
/// [`Placeholders::Fill`], POSIX: outside quotes the value as one quoted
/// word, inside double quotes with `\`, `"`, `$` and `` ` `` escaped, inside
/// single quotes with each `'` closed, escaped and reopened.
pub fn place_placeholders(
    kind: ShellKind,
    command: &str,
    find: &dyn Fn(&str) -> Option<usize>,
    mode: Placeholders,
    value: &dyn Fn(&str) -> String,
) -> String {
    #[derive(PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }
    let escape = if kind == ShellKind::Posix { '\\' } else { '`' };
    let single = |character: char| {
        character == '\''
            || (kind == ShellKind::PowerShell && ('\u{2018}'..='\u{201b}').contains(&character))
    };
    let mut out = String::with_capacity(command.len());
    let mut quote = Quote::None;
    let mut statement_start = true;
    // After PowerShell's own `&` or `.` the word is already called.
    let mut called = false;
    let mut rest = command;
    while let Some(character) = rest.chars().next() {
        if let Some(length) = find(rest) {
            let placeholder = &rest[..length];
            let name = placeholder_name(placeholder);
            match (&quote, kind, mode) {
                (Quote::None, ShellKind::PowerShell, _) if statement_start => {
                    // A path to run: quote the whole word and call it. The
                    // word ends at a space or a statement or block end that
                    // is not part of a placeholder.
                    let mut end = 0;
                    let mut word = String::new();
                    while end < rest.len() {
                        if let Some(inner) = find(&rest[end..]) {
                            let _ = write!(
                                word,
                                "${{env:{}}}",
                                placeholder_name(&rest[end..end + inner])
                            );
                            end += inner;
                            continue;
                        }
                        let next = rest[end..].chars().next().expect("not at the end");
                        if next.is_whitespace() || matches!(next, ';' | '|' | '&' | ')' | '}') {
                            break;
                        }
                        if next == '"' {
                            word.push_str("`\"");
                        } else {
                            word.push(next);
                        }
                        end += next.len_utf8();
                    }
                    let call = if called { "" } else { "& " };
                    let _ = write!(out, "{call}\"{word}\"");
                    rest = &rest[end..];
                    statement_start = false;
                    called = false;
                    continue;
                }
                (Quote::Single, ShellKind::Posix, Placeholders::Expand) => {
                    let _ = write!(out, "'\"{placeholder}\"'");
                }
                (Quote::Single, ShellKind::Posix, Placeholders::Fill) => {
                    out.push_str(&value(name).replace('\'', r"'\''"));
                }
                (Quote::Double, ShellKind::Posix, Placeholders::Fill) => {
                    for character in value(name).chars() {
                        if matches!(character, '\\' | '"' | '$' | '`') {
                            out.push('\\');
                        }
                        out.push(character);
                    }
                }
                (Quote::None, ShellKind::Posix, Placeholders::Fill) => {
                    out.push_str(&posix_word(&value(name)));
                }
                (Quote::Single, ShellKind::PowerShell, _) => {
                    let text = value(name);
                    if powershell_plain(&text) {
                        for character in text.chars() {
                            out.push(character);
                            if single(character) {
                                out.push(character);
                            }
                        }
                    } else {
                        let _ = write!(out, "${{env:{name}}}");
                    }
                }
                (_, ShellKind::Posix, Placeholders::Expand) => out.push_str(placeholder),
                (_, ShellKind::PowerShell, _) => {
                    let _ = write!(out, "${{env:{name}}}");
                }
            }
            rest = &rest[length..];
            statement_start = false;
            continue;
        }
        if quote == Quote::None && (rest.starts_with("&&") || rest.starts_with("||")) {
            out.push_str(&rest[..2]);
            rest = &rest[2..];
            statement_start = true;
            called = false;
            continue;
        }
        let mut length = character.len_utf8();
        match (&quote, character) {
            (Quote::None, _) if single(character) => quote = Quote::Single,
            (Quote::Single, _) if single(character) => quote = Quote::None,
            (Quote::Double, '"') => quote = Quote::None,
            (Quote::None, '"') => quote = Quote::Double,
            (Quote::None | Quote::Double, _) if character == escape => {
                // The escaped character goes with it, whatever it is.
                length += rest[length..].chars().next().map_or(0, char::len_utf8);
            }
            _ => {}
        }
        if quote == Quote::None {
            if matches!(character, ';' | '|' | '\n' | '{') {
                statement_start = true;
                called = false;
            } else if statement_start
                && matches!(character, '&' | '.')
                && rest[1..].starts_with(char::is_whitespace)
            {
                called = true;
            } else if !(character.is_whitespace() || (character == '&' && statement_start)) {
                statement_start = false;
                called = false;
            }
        } else {
            statement_start = false;
            called = false;
        }
        out.push_str(&rest[..length]);
        rest = &rest[length..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_variable_reference_is_found_with_its_fallback() {
        for (text, length) in [
            ("${HOME}/x", Some(7)),
            ("${TOKEN:-none} rest", Some(14)),
            ("${_A1}", Some(6)),
            ("${1}", None),
            ("${A B}", None),
            ("${A-x}", None),
            ("${UNCLOSED", None),
            ("$HOME", None),
        ] {
            assert_eq!(variable_at(text), length, "{text}");
        }
    }

    #[test]
    fn a_filled_value_is_plain_text_wherever_it_stands() {
        let value = |_: &str| "/p q/it's $(touch x) \"`a`\" \\".to_owned();
        let shown = |command: &str| {
            place_placeholders(
                ShellKind::Posix,
                command,
                &variable_at,
                Placeholders::Fill,
                &value,
            )
        };
        assert_eq!(
            shown("cat ${DIR}/x"),
            r#"cat '/p q/it'\''s $(touch x) "`a`" \'/x"#
        );
        assert_eq!(
            shown(r#"cat "${DIR}/x""#),
            r#"cat "/p q/it's \$(touch x) \"\`a\`\" \\/x""#
        );
        assert_eq!(
            shown("cat '${DIR}/x'"),
            r#"cat '/p q/it'\''s $(touch x) "`a`" \/x'"#
        );
        assert_eq!(
            place_placeholders(
                ShellKind::Posix,
                "cat ${DIR}/x",
                &variable_at,
                Placeholders::Fill,
                &|_| "/plain/path".to_owned(),
            ),
            "cat /plain/path/x"
        );
    }

    #[test]
    fn an_expanded_reference_stays_for_the_shell_and_opens_single_quotes() {
        let shown = |command: &str| {
            place_placeholders(
                ShellKind::Posix,
                command,
                &variable_at,
                Placeholders::Expand,
                &|_| unreachable!("the shell reads the value"),
            )
        };
        assert_eq!(
            shown("get-token --scope ${SCOPE} \"${A:-x}\""),
            "get-token --scope ${SCOPE} \"${A:-x}\""
        );
        assert_eq!(
            shown(r#"printf '{"X":"${TOKEN:-none}"}'"#),
            r#"printf '{"X":"'"${TOKEN:-none}"'"}'"#
        );
    }
}
