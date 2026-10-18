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

/// A URL with its user name, password and the values of credential-named
/// query parameters hidden; its scheme, host, path and other parameters
/// stay readable, so it still shows which server a setting points at.
/// Text that is not a URL is returned as it is.
#[must_use]
pub fn url(text: &str) -> Cow<'_, str> {
    let Ok(parsed) = url::Url::parse(text) else {
        return Cow::Borrowed(text);
    };
    let userinfo = !parsed.username().is_empty() || parsed.password().is_some();
    let query = parsed.query().map(|query| {
        query
            .split('&')
            .map(|pair| {
                let name = url::form_urlencoded::parse(pair.as_bytes())
                    .next()
                    .map(|(name, _)| name.into_owned())
                    .unwrap_or_default();
                if secret_name(&name) {
                    let raw_name = pair.split_once('=').map_or(pair, |(name, _)| name);
                    Cow::Owned(format!("{raw_name}={REDACTED}"))
                } else {
                    Cow::Borrowed(pair)
                }
            })
            .collect::<Vec<_>>()
    });
    let hides_query = query
        .as_ref()
        .is_some_and(|pairs| pairs.iter().any(|pair| matches!(pair, Cow::Owned(_))));
    if !userinfo && !hides_query {
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
    if let Some(pairs) = query {
        shown.push('?');
        shown.push_str(&pairs.join("&"));
    }
    if let Some(fragment) = parsed.fragment() {
        shown.push('#');
        shown.push_str(fragment);
    }
    Cow::Owned(shown)
}

/// `text` with every URL in it shown as [`url`] shows it, for error messages
/// that quote the address they could not reach.
#[must_use]
pub fn urls_in(text: &str) -> Cow<'_, str> {
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
    const VALUE: &str = r#""[^"\r\n]*"?|'[^'\r\n]*'?"#;
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
            (r"(?i)\b([a-z][a-z0-9+.-]*://)[^/\s@]+@".to_owned(), "${1}[redacted]@"),
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
    let mut text = text.to_owned();
    for (pattern, replacement) in RULES.iter() {
        if pattern.is_match(&text) {
            text = pattern.replace_all(&text, *replacement).into_owned();
        }
    }
    text
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
