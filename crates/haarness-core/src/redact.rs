//! Credentials hidden from what Haarness shows of a configuration: the MCP
//! settings that `haarness mcp get`, Settings and the model's `mcp_manage`
//! tool display, and the connection errors of a server. Only the display
//! changes; the configuration in use keeps its values.
//!
//! [`text`] and [`log_text`] are the free-text form, for log lines and
//! messages that quote a command, a URL or a hook's output.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

/// What a hidden value reads as. Saving a displayed setting unchanged keeps
/// the value it stands for.
pub const REDACTED: &str = "<redacted>";

/// Whether a setting, flag, header or query parameter of this name carries
/// a credential: API, access and private keys, secrets, passwords and
/// passphrases, credentials, tokens, authorization, cookies and signatures.
/// `pat`, `pass`, `pw`, `cred`, `keys` and `tokens` count only as a word of
/// their own (`GITHUB_PAT`, `dbPass`), never inside `pattern` or `passage`,
/// and a bare `pwd` is a working directory.
#[must_use]
pub fn secret_name(name: &str) -> bool {
    let words = words(name);
    let joined = words.concat();
    let Some(last) = words.last() else {
        return false;
    };
    [
        "apikey",
        "accesskey",
        "privatekey",
        "secret",
        "password",
        "passwd",
        "passphrase",
        "credential",
        "bearer",
    ]
    .iter()
    .any(|part| joined.contains(part))
        || [
            "key",
            "token",
            "auth",
            "authorization",
            "cookie",
            "cookies",
            "signature",
            "signatures",
        ]
        .iter()
        .any(|end| joined.ends_with(end))
        || joined == "sig"
        || ([
            "keys", "tokens", "cred", "creds", "pat", "pats", "pass", "pw",
        ]
        .contains(&last.as_str()))
        || (last == "pwd" && words.len() > 1)
}

/// The words of a name, lower case: `X-Api-Key`, `api_key` and `apiKey` are
/// all `api`, `key`.
fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for character in name.chars() {
        if !character.is_ascii_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            previous_lower = false;
            continue;
        }
        if character.is_ascii_uppercase() && previous_lower && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        previous_lower = character.is_ascii_lowercase() || character.is_ascii_digit();
        word.push(character.to_ascii_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Whether a URL query or fragment parameter of this name carries a
/// credential: any [`secret_name`], and also a whole `code` (an OAuth code,
/// a function key), `jwt`, `ticket`, `otp` or `totp`, an `auth`, `access` or
/// `mfa` code, and a name holding `hmac` or `passcode`.
fn secret_parameter(name: &str) -> bool {
    let joined = words(name).concat();
    secret_name(name)
        || ["code", "jwt", "ticket", "otp", "totp"].contains(&joined.as_str())
        || ["authcode", "accesscode", "mfacode"]
            .iter()
            .any(|end| joined.ends_with(end))
        || ["hmac", "passcode"]
            .iter()
            .any(|part| joined.contains(part))
}

/// `pairs` (`a=1&b=2`, a query or fragment) with the value of each
/// credential-named parameter hidden. Borrowed when nothing is hidden.
fn parameters(pairs: &str) -> Cow<'_, str> {
    let mut hidden = false;
    let shown: Vec<Cow<'_, str>> = pairs
        .split('&')
        .map(|pair| {
            let Some((raw_name, value)) = pair.split_once('=') else {
                return Cow::Borrowed(pair);
            };
            let name = url::form_urlencoded::parse(raw_name.as_bytes())
                .next()
                .map(|(name, _)| name.into_owned())
                .unwrap_or_default();
            if value.is_empty() || value == REDACTED || !secret_parameter(&name) {
                return Cow::Borrowed(pair);
            }
            hidden = true;
            Cow::Owned(format!("{raw_name}={REDACTED}"))
        })
        .collect();
    if hidden {
        Cow::Owned(shown.join("&"))
    } else {
        Cow::Borrowed(pairs)
    }
}

/// A URL with its user name, password and the values of credential-named
/// query and fragment parameters hidden; its scheme, host, path and other
/// parameters stay readable, so it still shows which server a setting points
/// at. A URL the parser refuses (a password holding a raw `/`, a bad port)
/// is hidden from after `://` up to its last `@`. Other text is returned as
/// it is.
#[must_use]
pub fn url(text: &str) -> Cow<'_, str> {
    let Ok(parsed) = url::Url::parse(text) else {
        return unparsed_url(text);
    };
    let userinfo = !parsed.username().is_empty() || parsed.password().is_some();
    let query = parsed.query().map(parameters);
    let fragment = parsed.fragment().map(parameters);
    let hides = |part: &Option<Cow<'_, str>>| matches!(part, Some(Cow::Owned(_)));
    if !userinfo && !hides(&query) && !hides(&fragment) {
        return Cow::Borrowed(text);
    }
    let mut shown = format!("{}:", parsed.scheme());
    if parsed.has_authority() {
        shown.push_str("//");
    }
    if userinfo {
        shown.push_str(REDACTED);
        shown.push('@');
    }
    shown.push_str(&parsed[url::Position::BeforeHost..url::Position::AfterPath]);
    if let Some(query) = query {
        shown.push('?');
        shown.push_str(&query);
    }
    if let Some(fragment) = fragment {
        shown.push('#');
        shown.push_str(&fragment);
    }
    Cow::Owned(shown)
}

/// A `scheme://…@…` text the URL parser refused, hidden from after `://` up
/// to its last `@`; anything else as it is.
fn unparsed_url(text: &str) -> Cow<'_, str> {
    let Some(start) = text.find("://") else {
        return Cow::Borrowed(text);
    };
    let scheme = &text[..start];
    let rest = &text[start + 3..];
    match rest.rfind('@') {
        Some(at)
            if at > 0
                && scheme.starts_with(|character: char| character.is_ascii_alphabetic())
                && scheme.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
                })
                && !rest[..at].contains(char::is_whitespace) =>
        {
            Cow::Owned(format!("{scheme}://{REDACTED}{}", &rest[at..]))
        }
        _ => Cow::Borrowed(text),
    }
}

/// `text` with each `url::Url` written the way Rust's `{:?}` writes one
/// (`Url { scheme: "https", …, username: "u", password: Some("p"), … }`, as
/// libraries log it) hidden as [`url`] hides a URL: the user name and
/// password, and credential-named query and fragment parameters.
fn debug_urls(text: &str) -> Cow<'_, str> {
    static FIELDS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"\b(username: |password: Some\(|query: Some\(|fragment: Some\()"((?:[^"\\]|\\.)*)""#,
        )
        .expect("static debug URL pattern")
    });
    if !text.contains("Url {") {
        return Cow::Borrowed(text);
    }
    let mut shown = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("Url {") {
        let end = start + debug_struct_end(&rest[start..]);
        shown.push_str(&rest[..start]);
        shown.push_str(
            &FIELDS.replace_all(&rest[start..end], |captures: &regex::Captures<'_>| {
                let (field, value) = (&captures[1], &captures[2]);
                let value = if field.starts_with("username") || field.starts_with("password") {
                    if value.is_empty() {
                        Cow::Borrowed(value)
                    } else {
                        Cow::Borrowed(REDACTED)
                    }
                } else {
                    parameters(value)
                };
                format!("{field}\"{value}\"")
            }),
        );
        rest = &rest[end..];
    }
    shown.push_str(rest);
    if shown == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(shown)
    }
}

/// Where the `{ … }` that `text` opens ends, past its closing brace, reading
/// quoted strings as Rust's `{:?}` writes them; the text's end when it was
/// cut off.
fn debug_struct_end(text: &str) -> usize {
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if quoted {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index + 1;
                }
            }
            _ => {}
        }
    }
    text.len()
}

/// `text` with every URL in it shown as [`url`] shows it, for error messages
/// that quote the address they could not reach. A URL written as Rust's
/// `{:?}` writes a `url::Url` is hidden the same way.
#[must_use]
pub fn urls_in(text: &str) -> Cow<'_, str> {
    let debug = debug_urls(text);
    match urls_in_text(&debug) {
        Cow::Owned(shown) => Cow::Owned(shown),
        Cow::Borrowed(_) => debug,
    }
}

fn urls_in_text(text: &str) -> Cow<'_, str> {
    let mut shown = String::new();
    let mut rest = text;
    let mut changed = false;
    while let Some(start) = rest.find("://") {
        let begin = rest[..start]
            .rfind(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.'))
            })
            .map_or(0, |index| index + 1);
        let end = rest[start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>' | ')')
            })
            .map_or(rest.len(), |index| start + index);
        // Punctuation that ends a sentence or a clause is not part of the URL.
        let end = start
            + rest[start..end]
                .trim_end_matches([':', ',', '.', ';', '!', '?'])
                .len();
        let candidate = &rest[begin..end];
        let hidden = url(candidate);
        changed |= matches!(hidden, Cow::Owned(_));
        shown.push_str(&rest[..begin]);
        shown.push_str(&hidden);
        rest = &rest[end..];
    }
    if !changed {
        return Cow::Borrowed(text);
    }
    shown.push_str(rest);
    Cow::Owned(shown)
}

/// The arguments of a command with credentials hidden: the value after a
/// credential flag (`--api-key VALUE`), a header flag (`-H VALUE`) or a
/// `user:password` flag (`-u VALUE`), the value of `--token=VALUE` and
/// `API_KEY=VALUE`, and any argument that is itself a bearer token, a JWT,
/// a private key or a URL carrying credentials.
#[must_use]
pub fn args(args: &[String]) -> Vec<String> {
    args.iter()
        .enumerate()
        .map(|(index, arg)| {
            let previous = index
                .checked_sub(1)
                .and_then(|previous| args.get(previous))
                .and_then(|previous| flag_name(previous));
            if previous.is_some_and(|flag| takes_credential(flag, arg)) {
                REDACTED.to_owned()
            } else {
                arg_value(arg).into_owned()
            }
        })
        .collect()
}

/// The name of a `-x` or `--name` flag that takes its value as the next
/// argument.
fn flag_name(arg: &str) -> Option<&str> {
    let name = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-'))?;
    (!name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')))
    .then_some(name)
}

/// Whether the argument after flag `name` is a credential. An argument that
/// is itself a flag is not a value, and a `--no-` flag takes none.
fn takes_credential(name: &str, value: &str) -> bool {
    if value.starts_with('-') {
        return false;
    }
    match name {
        "H" | "header" => true,
        "u" | "user" => value.contains(':'),
        _ => !name.starts_with("no-") && secret_name(name),
    }
}

fn arg_value(arg: &str) -> Cow<'_, str> {
    if arg.contains("-----BEGIN") && arg.contains("PRIVATE KEY-----") {
        return Cow::Borrowed(REDACTED);
    }
    if let Some((name, value)) = arg.split_once('=') {
        let bare = name.trim_start_matches('-');
        if !value.is_empty() && flag_name(&format!("--{bare}")).is_some() && secret_name(bare) {
            return Cow::Owned(format!("{name}={REDACTED}"));
        }
    }
    if let Some((name, value)) = arg.split_once(':')
        && !value.trim().is_empty()
        && !name.contains(char::is_whitespace)
        && !value.starts_with("//")
        && secret_name(name)
    {
        return Cow::Owned(format!("{name}: {REDACTED}"));
    }
    if bearer_or_jwt(arg) {
        return Cow::Borrowed(REDACTED);
    }
    urls_in(arg)
}

/// A bearer token of some length, or a JSON Web Token.
fn bearer_or_jwt(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if let Some(index) = lower.find("bearer ")
        && text[index + 7..]
            .split_whitespace()
            .next()
            .is_some_and(|token| token.len() >= 16)
    {
        return true;
    }
    text.split(|character: char| !(character.is_ascii_alphanumeric() || "-_.".contains(character)))
        .any(|word| {
            let parts: Vec<_> = word.split('.').collect();
            word.starts_with("eyJ") && parts.len() == 3 && parts.iter().all(|part| part.len() >= 8)
        })
}

/// Replaces obvious credential shapes in free text with `[redacted]`:
/// credential headers, `Bearer`/`Basic` values, curl user flags,
/// `key=value`, `key: value` and `--key value` pairs whose key names a
/// credential, URL user info, JWTs, well-known token prefixes and long opaque
/// runs. Over-redaction is fine; this is for log lines and messages, not data.
#[must_use]
pub fn text(text: &str) -> String {
    const KEYS: &str = r"token|secret|passw(?:or)?d|api[_-]?key|access[_-]?key|private[_-]?key|auth|cookie|credential|session";
    // A quoted value, or one written as Rust's `{:?}` writes `Some("…")`.
    const VALUE: &str = r#"Some\("(?:[^"\\\r\n]|\\.)*"?\)?|"[^"\r\n]*"?|'[^'\r\n]*'?"#;
    static RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        [
            // A cookie header can hold several credentials; drop the rest of its line.
            (r#"(?i)\b((?:set-)?cookie)(["']?[ \t]*:[ \t]*)[^\r\n]*"#.to_owned(), "${1}${2}[redacted]"),
            // Verbose curl repeats the identity outside its headers.
            (r"(?i)\b((?:server|proxy) auth using )[^\r\n]*".to_owned(), "${1}[redacted]"),
            // Any scheme, including `Token …` and `Digest …`.
            (
                format!(r#"(?i)\b((?:proxy-)?authorization)(["']?[ \t]*[:=][ \t]*)({VALUE}|[^\r\n"']*)"#),
                "${1}${2}[redacted]",
            ),
            (r#"(?i)\b(bearer|basic)[ \t]+[^\s"',;]+"#.to_owned(), "${1} [redacted]"),
            (
                format!(r#"(?i)(^|[^\w-])(--(?:proxy-)?user(?:=|[ \t]+))({VALUE}|[^\s"',;&]+)"#),
                "${1}${2}[redacted]",
            ),
            // `-u user:password`, but not `sort -u file`.
            (
                r#"(^|[^\w-])(-[uU][ \t]*)("[^"\r\n]*:[^"\r\n]*"?|'[^'\r\n]*:[^'\r\n]*'?|[^\s"',;&:]*:[^\s"',;&]*)"#.to_owned(),
                "${1}${2}[redacted]",
            ),
            (
                format!(r#"(?i)\b([\w.-]*(?:{KEYS})[\w.-]*)(["']?[ \t]*[=:][ \t]*)({VALUE}|[^\s"',;&]+)"#),
                "${1}${2}[redacted]",
            ),
            // `--token VALUE`, `-password VALUE`, as `set -x` traces print them.
            (
                format!(r#"(?i)(^|[^\w-])(--?[\w.-]*(?:{KEYS})[\w.-]*)[ \t]+({VALUE}|[^\s"',;&-][^\s"',;&]*)"#),
                "${1}${2} [redacted]",
            ),
            (r"\beyJ[\w-]{5,}\.[\w-]{5,}\.[\w-]*".to_owned(), "[redacted]"),
            (
                r"\b(?:gh[pousr]_|github_pat_|glpat-|xox[abeprs]-|sk-(?:ant-|proj-)?|AKIA|ASIA)[A-Za-z0-9_-]{12,}".to_owned(),
                "[redacted]",
            ),
            // Long opaque runs, without `/` or `.` so paths and hosts survive.
            (r"[A-Za-z0-9+_=-]{40,}".to_owned(), "[redacted]"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| {
            (Regex::new(&pattern).expect("static redaction pattern"), replacement)
        })
        .collect()
    });
    let mut text = userinfo(text).into_owned();
    for (pattern, replacement) in RULES.iter() {
        if pattern.is_match(&text) {
            text = pattern.replace_all(&text, *replacement).into_owned();
        }
    }
    text
}

/// `text` with each URL's user name and password replaced by `[redacted]`:
/// everything after `//` (or a JSON-escaped `\/\/`) up to the last `@`
/// before the host, as URL parsers read it, so a raw `@` in a password goes
/// too, and so does a backslash-escaped space. The host part ends at a
/// space, `/`, `?` or `#`, so the text is read in linear time.
///
/// A user name with no password counts only after a scheme other than
/// ssh's (where it names the account git logs in as), and only when it reads
/// as one word: a token can be one (`https://<token>@host`), while
/// `a//pkg@1.0` is a path and `"https://h","me@x"` a URL quoted beside an
/// address.
fn userinfo(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut shown = String::new();
    let mut copied = 0;
    let mut at = 0;
    while at + 1 < bytes.len() {
        let slashes = if bytes[at..].starts_with(b"//") {
            2
        } else if bytes[at..].starts_with(br"\/\/") {
            4
        } else {
            at += 1;
            continue;
        };
        let start = at + slashes;
        // The host part ends where a URL parser ends it, or where the text
        // the URL is written in ends (a quote or bracket), so an address
        // beside it (`"…","me@x"`) is never read as its user name.
        let mut end = authority_end(bytes, start, b"/?#\"`<>'");
        if !text[start..end].contains('@') && bytes.get(end) == Some(&b'\'') {
            // A `'` can belong to the user name (`o'neil`): read on past it.
            end = authority_end(bytes, start, b"/?#\"`<>");
        }
        if !text[start..end].contains('@')
            && port_is_not_a_number(&text[start..end])
            && matches!(bytes.get(end), Some(b'/' | b'?' | b'#'))
        {
            // `user:pa?ss@host`: a password holding a raw `/`, `?` or `#`
            // ends the host part early, before a "port" that is no number.
            // The user part runs to the first `@` after it, and the host to
            // the next `/`, `?` or `#`.
            // Read at most 2 KiB on, so a long run of such text stays linear.
            let region = authority_end(&bytes[..bytes.len().min(end + 2048)], end, b"\"`<>'");
            if let Some(first) = text[end..region].find('@') {
                let host = end + first + 1;
                end = text[host..region]
                    .find(['/', '?', '#'])
                    .map_or(region, |index| host + index);
            }
        }
        let authority = &text[start..end];
        if let Some(name_end) = authority.rfind('@')
            && !authority.starts_with('@')
            && user_credential(scheme_before(text, at), &authority[..name_end])
        {
            shown.push_str(&text[copied..start]);
            shown.push_str("[redacted]");
            copied = start + name_end;
        }
        at = end.max(at + 1);
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    shown.push_str(&text[copied..]);
    Cow::Owned(shown)
}

/// Where a URL's host part that starts at `from` ends: at a space (not a
/// backslash-escaped one), a JSON-escaped `\"`, or one of `stops`.
fn authority_end(bytes: &[u8], from: usize, stops: &[u8]) -> usize {
    let mut end = from;
    while let Some(&byte) = bytes.get(end) {
        match byte {
            b'\\' if bytes.get(end + 1).is_some_and(u8::is_ascii_whitespace) => end += 2,
            b'\\' if bytes.get(end + 1) == Some(&b'"') => break,
            _ if byte.is_ascii_whitespace() || stops.contains(&byte) => break,
            _ => end += 1,
        }
    }
    end
}

/// Whether what follows the last `:` of a host part is something other than
/// a port number (`user:pa` of `user:pa?ss@host`); an IPv6 address's own
/// colons do not count, and neither does a variable standing for the port
/// (`localhost:${PORT}`, `localhost:$PORT`).
fn port_is_not_a_number(authority: &str) -> bool {
    let host_port = authority
        .rsplit_once(']')
        .map_or(authority, |(_, after)| after);
    host_port.rsplit_once(':').is_some_and(|(_, port)| {
        !port.is_empty()
            && !port.starts_with('$')
            && !port.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// The scheme written right before the `//` at `slashes`, read only where a
/// run of scheme characters starts with a letter.
fn scheme_before(text: &str, slashes: usize) -> Option<&str> {
    let head = text[..slashes].strip_suffix(':')?;
    let start = head
        .rfind(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.'))
        })
        .map_or(0, |index| index + 1);
    let scheme = &head[start..];
    scheme
        .starts_with(|character: char| character.is_ascii_alphabetic())
        .then_some(scheme)
}

/// Whether the user part `name` of a URL holds a credential (see
/// [`userinfo`]). What the URL display already hid stays as written.
fn user_credential(scheme: Option<&str>, name: &str) -> bool {
    if name.is_empty() || name == "[redacted]" || name == REDACTED {
        return false;
    }
    name.contains(':')
        || (scheme.is_some_and(|scheme| {
            !["ssh", "git+ssh", "ssh+git"]
                .iter()
                .any(|ssh| scheme.eq_ignore_ascii_case(ssh))
        }) && !name.contains([
            '"', '\'', '<', '>', ',', ';', '(', ')', '[', ']', '{', '}', '|', '`', '\\',
        ]))
}

/// At most `max` characters, ending with `…` when cut. Never splits a char.
#[must_use]
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(max.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// Free text made safe for one log field: redacted as [`text`] does, with
/// C0/C1 control characters and line breaks collapsed to single spaces, then
/// clipped to `max` characters.
#[must_use]
pub fn log_text(value: &str, max: usize) -> String {
    let redacted = text(value);
    let words: Vec<&str> = redacted
        .split(|character: char| character.is_whitespace() || character.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    clip(&words.join(" "), max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_names_are_recognised_without_catching_ordinary_words() {
        for name in [
            "api_key",
            "X-Api-Key",
            "apiKey",
            "ANTHROPIC_API_KEY",
            "access-token",
            "token",
            "client_secret",
            "password",
            "dbPass",
            "GITHUB_PAT",
            "Authorization",
            "cookie",
            "API_TOKENS",
            "exaApiKey",
            "sig",
        ] {
            assert!(secret_name(name), "{name}");
        }
        for name in [
            "model", "pattern", "passage", "pwd", "timeout", "url", "region", "port", "verbose",
        ] {
            assert!(!secret_name(name), "{name}");
        }
    }

    #[test]
    fn a_url_keeps_its_host_and_hides_only_its_credentials() {
        assert_eq!(
            url("https://user:hunter2@mcp.example.com/sse?api_key=sk-1&region=eu#top"),
            "https://<redacted>@mcp.example.com/sse?api_key=<redacted>&region=eu#top"
        );
        assert_eq!(
            url("https://mcp.exa.ai/mcp?exaApiKey=abc123"),
            "https://mcp.exa.ai/mcp?exaApiKey=<redacted>"
        );
        for plain in [
            "https://mcp.example.com/mcp?region=eu",
            "http://127.0.0.1:9/mcp",
            "not a url",
            "${MCP_URL}",
        ] {
            assert!(matches!(url(plain), Cow::Borrowed(_)), "{plain}");
        }
        assert_eq!(
            urls_in("cannot reach https://mcp.example.com/?token=abc: Connection refused"),
            "cannot reach https://mcp.example.com/?token=<redacted>: Connection refused"
        );
        assert!(matches!(
            urls_in("cannot reach http://127.0.0.1:9/mcp: Connection refused"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn command_arguments_hide_credentials_and_keep_the_rest() {
        let given = [
            "-y",
            "@acme/mcp",
            "--api-key",
            "sk-live-1",
            "--token=abc",
            "-H",
            "Authorization: Bearer abcdefghijklmnopqrstu",
            "--no-auth",
            "--verbose",
            "API_KEY=xyz",
            "--url",
            "https://bob:pw@api.example.com/v1",
            "--path",
            "/a b",
            "--flag=x=y",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJlLXZhbHVl",
        ]
        .map(str::to_owned);
        assert_eq!(
            args(&given),
            [
                "-y",
                "@acme/mcp",
                "--api-key",
                REDACTED,
                "--token=<redacted>",
                "-H",
                REDACTED,
                "--no-auth",
                "--verbose",
                "API_KEY=<redacted>",
                "--url",
                "https://<redacted>@api.example.com/v1",
                "--path",
                "/a b",
                "--flag=x=y",
                REDACTED,
            ]
        );
    }

    #[test]
    fn free_text_hides_credential_shapes_and_keeps_ordinary_text() {
        for (input, secret) in [
            ("--token=PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            ("gh --password PRIVATE_SUFFIX api", "PRIVATE_SUFFIX"),
            ("tool -password 'PRIVATE SUFFIX'", "PRIVATE SUFFIX"),
            ("--api-key PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            (
                "curl -H 'Authorization: Bearer PRIVATE.SUFFIX' https://x.test",
                "PRIVATE.SUFFIX",
            ),
            ("Authorization: Token PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            (
                "proxy-authorization=Digest PRIVATE_SUFFIX",
                "PRIVATE_SUFFIX",
            ),
            ("bearer PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            ("Basic UFJJVkFURV9TVUZGSVg=", "UFJJVkFURV9TVUZGSVg="),
            (
                r#"{"api_key": "PRIVATE SUFFIX", "ok": 1}"#,
                "PRIVATE SUFFIX",
            ),
            (r#"{"password":"PRIVATE_SUFFIX"}"#, "PRIVATE_SUFFIX"),
            ("API_KEY=PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            ("client_secret: PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            ("private-key=PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            ("passwd=PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            ("session=PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            ("credentials: PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            ("Cookie: a=1; sid=PRIVATE_SUFFIX", "PRIVATE_SUFFIX"),
            (
                "* Server auth using Basic with user 'PRIVATE_SUFFIX'",
                "PRIVATE_SUFFIX",
            ),
            (
                "https://alice:PRIVATE_SUFFIX@git.example.test/repo",
                "PRIVATE_SUFFIX",
            ),
            (
                "https://PRIVATE_SUFFIX@github.com/org/repo",
                "PRIVATE_SUFFIX",
            ),
            (
                "GET /cb?code=1&access_token=PRIVATE_SUFFIX&x=1",
                "PRIVATE_SUFFIX",
            ),
            (
                "curl -u alice:PRIVATE_SUFFIX https://x.test",
                "PRIVATE_SUFFIX",
            ),
            ("curl --user 'alice:PRIVATE SUFFIX'", "PRIVATE SUFFIX"),
            ("password: \"PRIVATE_SUFFIX\nnext line", "PRIVATE_SUFFIX"),
            ("token=秘密の値 rest", "秘密の値"),
            ("ghp_PRIVATESUFFIX0123456789", "PRIVATESUFFIX"),
            ("key sk-ant-PRIVATESUFFIX01234", "PRIVATESUFFIX"),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJQUklWQVRFIn0.c2ln",
                "eyJzdWIiOiJQUklWQVRFIn0",
            ),
            (
                "x PRIVATESUFFIX0123456789abcdefghijklmnopqrstuv x",
                "PRIVATESUFFIX",
            ),
        ] {
            let redacted = text(input);
            assert!(!redacted.contains(secret), "{input:?} -> {redacted:?}");
            assert!(redacted.contains("[redacted]"), "{input:?} -> {redacted:?}");
        }
        for ordinary in [
            "build failed: missing file src/lib.rs",
            "mkdir -p out && sort -u names.txt",
            "listening on 127.0.0.1:4096",
            "authentication failed for user",
            "日本語のエラー: ファイルがありません",
        ] {
            assert_eq!(text(ordinary), ordinary);
        }
        assert_eq!(
            text("--token=a\n--token b"),
            "--token=[redacted]\n--token [redacted]"
        );
    }

    #[test]
    fn free_text_hides_a_url_user_and_password_up_to_the_last_at_before_the_host() {
        for (input, output) in [
            ("http://user:p@ss3cret@proxy", "http://[redacted]@proxy"),
            (
                "upstream http://user:p@ss@w0rd@proxy.fixture.test:3128/x failed",
                "upstream http://[redacted]@proxy.fixture.test:3128/x failed",
            ),
            (
                "https://me@corp.fixture.test:pw@h.fixture.test/x",
                "https://[redacted]@h.fixture.test/x",
            ),
            (
                "socks5://:fake-pw-0000@proxy.fixture.test:1080",
                "socks5://[redacted]@proxy.fixture.test:1080",
            ),
            (
                "https://fake-tok-0000@h.fixture.test/org/repo.git",
                "https://[redacted]@h.fixture.test/org/repo.git",
            ),
            (
                "http://user:p@ss@[::1]:8080/x",
                "http://[redacted]@[::1]:8080/x",
            ),
            (
                "//user:p@ss@h.fixture.test/",
                "//[redacted]@h.fixture.test/",
            ),
            (
                r#"{"url":"https:\/\/u:p@ss@h.fixture.test\/x"}"#,
                r#"{"url":"https:\/\/[redacted]@h.fixture.test\/x"}"#,
            ),
            (
                r"git clone https://u:pa\ ss@h.fixture.test/r",
                "git clone https://[redacted]@h.fixture.test/r",
            ),
            (
                "ssh://git:p@ss@h.fixture.test/r.git",
                "ssh://[redacted]@h.fixture.test/r.git",
            ),
            // A raw `?`, `#` or `/` in the password.
            (
                "proxy http://user:pa?ss@proxy:3128 refused",
                "proxy http://[redacted]@proxy:3128 refused",
            ),
            (
                "http://user:pa#ss@proxy:3128/x",
                "http://[redacted]@proxy:3128/x",
            ),
            (
                "http://user:pa/ss@h.fixture.test/path@v1",
                "http://[redacted]@h.fixture.test/path@v1",
            ),
            // A token in compact JSON, beside an address.
            (
                r#"["https://TOKEN123@h.fixture.test/x","me@x.fixture.test"]"#,
                r#"["https://[redacted]@h.fixture.test/x","me@x.fixture.test"]"#,
            ),
            (
                "https://o'neil:pw@h.fixture.test/x",
                "https://[redacted]@h.fixture.test/x",
            ),
        ] {
            assert_eq!(text(input), output, "{input}");
            assert_eq!(text(output), output, "{output}");
        }
        for kept in [
            "mailto:a@b",
            "npm:pkg@1.0",
            "someone@h.fixture.test",
            "http://@h.fixture.test/",
            "://@",
            "https://h.fixture.test/a//pkg@1.0/x",
            "cache//bun-darwin-aarch64@1.4.2@@@1/bin/bun",
            "https://registry.fixture.test/@scope%2fpkg",
            // The account git logs in as, never a credential.
            "ssh://git@h.fixture.test/org/repo.git",
            "git+ssh://git@h.fixture.test/org/repo.git",
            // A URL quoted beside an address.
            r#"["https://h.fixture.test","me@x.fixture.test"]"#,
            // What the URL display already hid.
            "https://<redacted>@h.fixture.test/x",
            // An address beside a URL in compact JSON, and query addresses.
            r#"{"url":"http://localhost:3000","email":"me@example.com"}"#,
            r#"{\"url\":\"http://localhost:3000\",\"email\":\"me@example.com\"}"#,
            "https://h.fixture.test:8080/x?email=a@b",
            "http://[::1]/x?a@b",
            // A variable standing for the port.
            "http://localhost:${PORT}/api/@scope/pkg",
            "http://localhost:$PORT/api/@scope/pkg",
            "['https://h.fixture.test','me@x.fixture.test']",
        ] {
            assert_eq!(text(kept), kept);
        }
    }

    #[test]
    fn free_text_reads_long_runs_of_slashes_and_authorities_quickly() {
        let started = std::time::Instant::now();
        for run in [
            "//a".repeat(100_000),
            format!("https://{}", "a:".repeat(100_000)),
            format!("https://{}", "\\ ".repeat(100_000)),
            format!("{}://x", "z".repeat(200_000)),
            "//a:b/".repeat(100_000),
        ] {
            let run = format!("{run} @");
            assert_eq!(userinfo(&run), run.as_str());
        }
        assert_eq!(
            userinfo(&format!("https://{}h.fixture.test/", "a@".repeat(100_000))),
            "https://[redacted]@h.fixture.test/"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_url_hides_credential_parameters_in_its_query_and_fragment_and_a_url_that_does_not_parse() {
        assert_eq!(
            url(
                "https://h.fixture.test/x?pw=1&code=2&jwt=3&otp=4&mfa_code=5&x-hmac=6&passcode=7&state=s"
            ),
            "https://h.fixture.test/x?pw=<redacted>&code=<redacted>&jwt=<redacted>&otp=<redacted>&mfa_code=<redacted>&x-hmac=<redacted>&passcode=<redacted>&state=s"
        );
        assert_eq!(
            url("https://h.fixture.test/cb#access_token=PRIVATE&state=s"),
            "https://h.fixture.test/cb#access_token=<redacted>&state=s"
        );
        for kept in [
            "https://h.fixture.test/docs#install",
            "https://h.fixture.test/?keyword=hello&compass=2",
        ] {
            assert!(matches!(url(kept), Cow::Borrowed(_)), "{kept}");
        }
        // A raw `/` in the password ends the host part for the parser, which
        // then refuses the port.
        assert_eq!(
            url("https://user:pa/ss@h.fixture.test/x"),
            "https://<redacted>@h.fixture.test/x"
        );
        assert_eq!(
            urls_in("cannot reach https://user:pa/ss@h.fixture.test/x: refused"),
            "cannot reach https://<redacted>@h.fixture.test/x: refused"
        );
    }

    #[test]
    fn a_url_written_as_rust_debug_output_hides_its_credentials() {
        let line = r#"discovery url: Url { scheme: "https", cannot_be_a_base: false, username: "PRIVATE_USER", password: Some("PRIVATE_PASS"), host: Some(Domain("mcp.fixture.test")), port: None, path: "/.well-known/oauth-authorization-server", query: Some("api_key=PRIVATE_KEY&region=eu"), fragment: None } next"#;
        let shown = urls_in(line);
        for secret in ["PRIVATE_USER", "PRIVATE_PASS", "PRIVATE_KEY"] {
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(
            shown.contains(r#"username: "<redacted>", password: Some("<redacted>"), host: Some(Domain("mcp.fixture.test"))"#),
            "{shown}"
        );
        assert!(
            shown.contains(r#"query: Some("api_key=<redacted>&region=eu"), fragment: None } next"#),
            "{shown}"
        );
        let plain = r#"Url { scheme: "https", cannot_be_a_base: false, username: "", password: None, host: Some(Domain("h")), port: None, path: "/", query: None, fragment: None }"#;
        assert!(matches!(urls_in(plain), Cow::Borrowed(_)));
        // The free-text masker hides a debug-written password whole too.
        assert_eq!(
            text(r#"password: Some("PRIVATE PASS"), host"#),
            "password: [redacted], host"
        );
    }

    #[test]
    fn log_text_strips_controls_and_clips_on_char_boundaries() {
        assert_eq!(
            log_text("a\u{1b}[31mb\u{7}\r\nc\u{85}d\u{9b}e\u{7f}f\t g", 100),
            "a [31mb c d e f g"
        );
        assert_eq!(clip("日本語テキスト", 4), "日本語…");
        assert_eq!(clip("日本語", 3), "日本語");
        assert_eq!(clip("🙂🙂🙂🙂", 2), "🙂…");
        for max in 0..12 {
            let clipped = log_text("é🙂 token=秘密 ü", max);
            assert!(clipped.chars().count() <= max.max(1), "{max}: {clipped:?}");
            assert!(!clipped.contains("秘密"));
        }
    }
}
