//! Values that go into the text a shell parses: a hook's command, an MCP
//! server's `headersHelper`, a command's or skill's `` !`…` `` snippet. A
//! value pasted into that text as it is becomes shell syntax: a folder named
//! `p$(touch x)` runs `touch`, and one holding a space splits a word. So the
//! value goes in the command's environment and the text keeps its
//! `${NAME}` reference for the shell to expand. The scanner here reads the
//! text as the shell reads its quotes, to open a reference written inside
//! single quotes (where a shell expands nothing) and, for PowerShell, to
//! write references the way it reads them.

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

/// Whether a value can be written inside PowerShell single quotes as it is:
/// only characters no PowerShell reads specially there.
fn powershell_plain(value: &str) -> bool {
    value.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || matches!(character, ' ' | '_' | '.' | '/' | ':' | '\\' | '-')
    })
}

/// `command` with each placeholder `find` reports (its length at the start
/// of the text) left for the shell to expand from the environment the
/// command gets, as the scanner reads the shell's quotes:
/// - outside single quotes a POSIX shell's `${NAME}` stays and PowerShell's
///   becomes `${env:NAME}`;
/// - inside single quotes, where nothing expands, POSIX gets `'"${NAME}"'`
///   (close, expand in double quotes, reopen); where the scanner misjudges
///   the quotes the value is still never part of the text, only placed
///   wrong. A POSIX command's `#` comments, `$'…'` strings and the quotes of
///   each `$(…)` and `` `…` `` are read as the shell reads them;
/// - PowerShell gets the value itself inside single quotes only when it is
///   made of plain characters, every single-quote character (ASCII and
///   U+2018–U+201B) doubled, and `${env:NAME}` otherwise, which is then read
///   literally: wrong, but never run. Each statement that starts with a
///   placeholder (at the start, after `;`, `|`, `&&`, `||` or a line break)
///   is a path to run, so that word is quoted and called with `&`.
pub fn place_placeholders(
    kind: ShellKind,
    command: &str,
    find: &dyn Fn(&str) -> Option<usize>,
    value: &dyn Fn(&str) -> String,
) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }
    /// A command substitution the scanner is inside: the quote it opened
    /// in, how deep its own parentheses go, and whether it is a backtick one.
    struct Frame {
        outer: Quote,
        depth: usize,
        backtick: bool,
    }
    let posix = kind == ShellKind::Posix;
    let escape = if posix { '\\' } else { '`' };
    let single = |character: char| {
        character == '\''
            || (kind == ShellKind::PowerShell && ('\u{2018}'..='\u{201b}').contains(&character))
    };
    let mut out = String::with_capacity(command.len());
    let mut quote = Quote::None;
    let mut frames: Vec<Frame> = Vec::new();
    // Here-documents whose bodies start on the next line.
    let mut pending: Vec<Heredoc> = Vec::new();
    let mut previous: Option<char> = None;
    let mut statement_start = true;
    // After PowerShell's own `&` or `.` the word is already called.
    let mut called = false;
    let mut rest = command;
    while let Some(character) = rest.chars().next() {
        if let Some(length) = find(rest) {
            let placeholder = &rest[..length];
            let name = placeholder_name(placeholder);
            match (quote, kind) {
                (Quote::None, ShellKind::PowerShell) if statement_start => {
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
                    previous = Some('"');
                    continue;
                }
                (Quote::Single, ShellKind::Posix) => {
                    let _ = write!(out, "'\"{placeholder}\"'");
                }
                (Quote::Single, ShellKind::PowerShell) => {
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
                (_, ShellKind::Posix) => out.push_str(placeholder),
                (_, ShellKind::PowerShell) => {
                    let _ = write!(out, "${{env:{name}}}");
                }
            }
            rest = &rest[length..];
            statement_start = false;
            previous = Some('}');
            continue;
        }
        if posix && quote == Quote::None {
            // A comment runs to the end of its line; its quotes are text.
            let word_start = previous.is_none_or(|before| {
                before.is_whitespace() || matches!(before, ';' | '&' | '|' | '(' | ')')
            });
            if character == '#' && word_start {
                let end = rest.find('\n').unwrap_or(rest.len());
                out.push_str(&rest[..end]);
                rest = &rest[end..];
                previous = Some('#');
                continue;
            }
            // `$'…'` closes only at an unescaped quote and expands nothing.
            if rest.starts_with("$'") {
                let mut characters = rest.char_indices().skip(2);
                let mut end = rest.len();
                while let Some((index, character)) = characters.next() {
                    match character {
                        '\\' => {
                            characters.next();
                        }
                        '\'' => {
                            end = index + 1;
                            break;
                        }
                        _ => {}
                    }
                }
                out.push_str(&rest[..end]);
                rest = &rest[end..];
                previous = Some('\'');
                continue;
            }
            // A here-string is no here-document.
            if rest.starts_with("<<<") {
                out.push_str("<<<");
                rest = &rest[3..];
                previous = Some('<');
                continue;
            }
            // A here-document's body starts on the next line.
            if rest.starts_with("<<")
                && let Some((length, heredoc)) = heredoc_operator(rest)
            {
                out.push_str(&rest[..length]);
                rest = &rest[length..];
                pending.push(heredoc);
                previous = Some('_');
                continue;
            }
            // Its body is no shell text: a quoted one (`<<'EOF'`) expands
            // nothing, so a placeholder gets its value there; an unquoted
            // one expands references but its quotes are text, so it stays as
            // it is.
            if character == '\n' && !pending.is_empty() {
                out.push('\n');
                rest = &rest[1..];
                for heredoc in pending.drain(..) {
                    while !rest.is_empty() {
                        let end = rest.find('\n').map_or(rest.len(), |index| index + 1);
                        let line = &rest[..end];
                        let bare = line.trim_end_matches('\n');
                        let bare = if heredoc.strip_tabs {
                            bare.trim_start_matches('\t')
                        } else {
                            bare
                        };
                        rest = &rest[end..];
                        if bare == heredoc.delimiter {
                            out.push_str(line);
                            break;
                        }
                        if heredoc.quoted {
                            out.push_str(&filled(line, find, value));
                        } else {
                            out.push_str(line);
                        }
                    }
                }
                previous = Some('\n');
                statement_start = true;
                called = false;
                continue;
            }
        }
        if posix && quote != Quote::Single {
            // A command substitution starts its own quoting.
            let backtick_closes = character == '`'
                && quote == Quote::None
                && frames.last().is_some_and(|frame| frame.backtick);
            if rest.starts_with("$(") || (character == '`' && !backtick_closes) {
                let length = if character == '`' { 1 } else { 2 };
                frames.push(Frame {
                    outer: quote,
                    depth: 0,
                    backtick: character == '`',
                });
                quote = Quote::None;
                out.push_str(&rest[..length]);
                rest = &rest[length..];
                previous = Some('(');
                continue;
            }
            if quote == Quote::None
                && let Some(frame) = frames.last_mut()
            {
                let closes = if frame.backtick {
                    backtick_closes
                } else if character == '(' {
                    frame.depth += 1;
                    false
                } else if character == ')' {
                    if frame.depth == 0 {
                        true
                    } else {
                        frame.depth -= 1;
                        false
                    }
                } else {
                    false
                };
                if closes {
                    quote = frames.pop().map_or(Quote::None, |frame| frame.outer);
                    out.push(character);
                    rest = &rest[character.len_utf8()..];
                    previous = Some(character);
                    continue;
                }
            }
        }
        if quote == Quote::None && (rest.starts_with("&&") || rest.starts_with("||")) {
            out.push_str(&rest[..2]);
            rest = &rest[2..];
            statement_start = true;
            called = false;
            previous = Some('&');
            continue;
        }
        let mut length = character.len_utf8();
        match (quote, character) {
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
        previous = rest[..length].chars().last();
        rest = &rest[length..];
    }
    out
}

/// A here-document's operator: its delimiter, whether that was quoted (so
/// the body expands nothing) and whether `<<-` strips the body's tabs.
struct Heredoc {
    delimiter: String,
    quoted: bool,
    strip_tabs: bool,
}

/// The `<<word` or `<<-word` at the start of `rest`: its length and what it
/// opens, or `None` when no delimiter follows.
fn heredoc_operator(rest: &str) -> Option<(usize, Heredoc)> {
    let mut index = 2;
    let strip_tabs = rest[index..].starts_with('-');
    if strip_tabs {
        index += 1;
    }
    index += rest[index..].len() - rest[index..].trim_start_matches([' ', '\t']).len();
    let mut delimiter = String::new();
    let mut quoted = false;
    let mut end = rest.len();
    let mut characters = rest[index..].char_indices();
    while let Some((offset, character)) = characters.next() {
        match character {
            '\'' | '"' => {
                quoted = true;
                let mut closed = false;
                for (_, inner) in characters.by_ref() {
                    if inner == character {
                        closed = true;
                        break;
                    }
                    delimiter.push(inner);
                }
                if !closed {
                    return None;
                }
            }
            '\\' => {
                quoted = true;
                if let Some((_, next)) = characters.next() {
                    delimiter.push(next);
                }
            }
            _ if character.is_whitespace() || ";|&<>()".contains(character) => {
                end = index + offset;
                break;
            }
            _ => delimiter.push(character),
        }
    }
    (!delimiter.is_empty()).then_some((
        end,
        Heredoc {
            delimiter,
            quoted,
            strip_tabs,
        },
    ))
}

/// `text` with each placeholder `find` reports replaced by its value.
fn filled(
    text: &str,
    find: &dyn Fn(&str) -> Option<usize>,
    value: &dyn Fn(&str) -> String,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        match find(rest) {
            Some(length) => {
                out.push_str(&value(placeholder_name(&rest[..length])));
                rest = &rest[length..];
            }
            None => {
                out.push_str("${");
                rest = &rest[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expanded(command: &str) -> String {
        place_placeholders(ShellKind::Posix, command, &variable_at, &|_| {
            unreachable!("the shell reads the value")
        })
    }

    /// A here-document's body is no shell text: a quoted one gets the value,
    /// an unquoted one stays as it is, quotes included.
    #[test]
    fn a_heredoc_body_is_read_as_the_shell_reads_it() {
        let shown = |command: &str| {
            place_placeholders(ShellKind::Posix, command, &variable_at, &|_| "/p q".into())
        };
        assert_eq!(
            shown("cat <<'EOF'\npath ${DIR}\nEOF\necho \"${DIR}\""),
            "cat <<'EOF'\npath /p q\nEOF\necho \"${DIR}\""
        );
        assert_eq!(
            shown("cat <<EOF\nit's ${DIR}\nEOF\nprintf '%s' '${DIR}'"),
            "cat <<EOF\nit's ${DIR}\nEOF\nprintf '%s' ''\"${DIR}\"''"
        );
        assert_eq!(
            shown("cat <<- \"END\" | sort\n\tin ${DIR}\n\tEND\necho ${DIR}"),
            "cat <<- \"END\" | sort\n\tin /p q\n\tEND\necho ${DIR}"
        );
        // A here-string is no here-document.
        assert_eq!(shown("cat <<< '${DIR}'"), "cat <<< ''\"${DIR}\"''");
    }

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
    fn a_reference_stays_for_the_shell_and_opens_single_quotes() {
        for (command, shown) in [
            (
                "get-token --scope ${SCOPE} \"${A:-x}\"",
                "get-token --scope ${SCOPE} \"${A:-x}\"",
            ),
            (
                r#"printf '{"X":"${TOKEN:-none}"}'"#,
                r#"printf '{"X":"'"${TOKEN:-none}"'"}'"#,
            ),
            // The quotes of a command substitution are its own.
            (r#"cd "$(dirname "${DIR}")""#, r#"cd "$(dirname "${DIR}")""#),
            (
                r#"echo "$(printf '%s' '${DIR}')""#,
                r#"echo "$(printf '%s' ''"${DIR}"'')""#,
            ),
            ("echo `printf '${DIR}'`", "echo `printf ''\"${DIR}\"''`"),
            // An apostrophe in a comment opens no quote.
            (
                "true # it's here\nprintf %s ${DIR}",
                "true # it's here\nprintf %s ${DIR}",
            ),
            // `$'…'` ends at its own unescaped quote.
            ("printf $'it\\'s' ${DIR}", "printf $'it\\'s' ${DIR}"),
            ("echo ${#DIR} ${DIR}", "echo ${#DIR} ${DIR}"),
            ("printf $'\\é' '${DIR}'", "printf $'\\é' ''\"${DIR}\"''"),
        ] {
            assert_eq!(expanded(command), shown, "{command}");
        }
    }
}
