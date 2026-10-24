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
/// a credential: API, access and private keys, secrets, passwords,
/// passphrases and passcodes, credentials, tokens, authorization, cookies
/// and signatures. `pat`, `pass`, `pw`, `cred`, `keys` and `tokens` count
/// only as a word of their own (`GITHUB_PAT`, `dbPass`), or a `pass` glued
/// to an account (`storepass`, `keypass`), never inside `pattern` or
/// `passage`, and a bare `pwd` is a working directory.
#[must_use]
pub fn secret_name(name: &str) -> bool {
    let words = words(name);
    let joined = words.concat();
    let Some(last) = words.last() else {
        return false;
    };
    passcode(&words)
        || [
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
        || account_pass(last)
}

/// Whether the words of a name end in a passcode or a passphrase, written as
/// one word or two (`passcode`, `devicePasscode`, `pass_code`,
/// `PASS_PHRASE`); `bypass_code` holds neither, and `passcode_length` is
/// its length.
fn passcode(words: &[String]) -> bool {
    words
        .last()
        .is_some_and(|word| word.ends_with("passcode") || word.ends_with("passphrase"))
        || (words.len() > 1
            && words[words.len() - 2] == "pass"
            && matches!(words[words.len() - 1].as_str(), "code" | "phrase"))
}

/// The accounts a glued `pass` or `pw` opens (`dbpass`, `rootpw`); any
/// other glued `pass` is a word (`bypass`, `compass`, `trespass`).
const ACCOUNTS: [&str; 16] = [
    "user", "db", "admin", "root", "login", "proxy", "ftp", "smtp", "mail", "ldap", "ssh", "mysql",
    "pg", "redis", "store", "key",
];

/// Whether `word` is a `pass` or `pw` glued to one of [`ACCOUNTS`].
fn account_pass(word: &str) -> bool {
    ["pass", "pw"].iter().any(|end| {
        word.strip_suffix(end)
            .is_some_and(|start| ACCOUNTS.iter().any(|account| start.ends_with(account)))
    })
}

/// Whether a name in free text is a credential's that the `KEYS` rules of
/// [`text`] do not catch: a passcode or passphrase (see [`passcode`]), a
/// last word `pass`, `pw` or `pwd` after another word (`DB_PASS`, `dbPass`,
/// `MYSQL_PWD`, `x-pw`), or a `pass` or `pw` glued to an account
/// (`DBPASS`). A bare `pass`, `pw` or `pwd` stays readable, as in
/// `tests pass: 3` or `PWD=/home/me`.
fn pass_name(name: &str) -> bool {
    let words = words(name);
    let Some(last) = words.last() else {
        return false;
    };
    passcode(&words)
        || (words.len() > 1 && matches!(last.as_str(), "pass" | "pw" | "pwd"))
        || account_pass(last)
}

/// Whether a [`pass_name`] names an account's password outright: a passcode
/// or passphrase, or a `pass`, `pw` or `pwd` of an account (`DB_PASS`,
/// `MYSQL_ROOT_PWD`, `storepass`). Only those hide a number: `tests_pass=12`
/// and `num_pass: 7` count tests.
fn account_password_name(name: &str) -> bool {
    let words = words(name);
    let Some(last) = words.last() else {
        return false;
    };
    passcode(&words)
        || account_pass(last)
        || (words.len() > 1
            && matches!(last.as_str(), "pass" | "pw" | "pwd")
            && ACCOUNTS
                .iter()
                .any(|account| words[words.len() - 2].ends_with(account)))
}

/// Whether a [`pass_name`] flag switches something rather than takes a
/// password (`--skip-pass`, `--ask-pass`, `--no-pass`): what follows it is
/// the next argument.
fn switch_name(name: &str) -> bool {
    let words = words(name);
    words.len() > 1
        && [
            "skip", "no", "ask", "allow", "require", "force", "check", "test", "tests", "lint",
        ]
        .contains(&words[0].as_str())
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

/// Words that say what a glued key opens or who holds it (`apikeys`,
/// `sshkeys`).
const HOLDERS: [&str; 17] = [
    "api",
    "auth",
    "app",
    "access",
    "private",
    "priv",
    "secret",
    "session",
    "master",
    "encryption",
    "signing",
    "subscription",
    "license",
    "client",
    "shared",
    "account",
    "ssh",
];

/// Whether a URL query or fragment parameter of this name, holding `value`,
/// carries a credential: any [`secret_name`], and also a whole `code` (an
/// OAuth code, a function key) or `ticket`, a whole or last word `jwt`,
/// `bearer`, `otp` or `totp` (`x-otp`), an `auth`, `access` or `mfa` code,
/// and a name holding `hmac` or `passcode`.
///
/// Parameter names glue their words together more often than settings do,
/// so these count too: a last word that ends in `pwd` (`pwd`, `adminpwd`,
/// `userpwds`), a `pass` or `pw` glued to the account it opens (`userpass`,
/// `dbpass`, `rootpw`), keys glued to what they open (`apikeys`), and tokens
/// glued to the word before them (`apitokens`) unless the value counts them
/// (`maxtokens=4096`). Any other glued `pass` stays readable (`bypass`,
/// `compass`, `trespass`), and so do `keyword` and `show_pw_hint`.
fn secret_parameter(name: &str, value: &str) -> bool {
    let words = words(name);
    let joined = words.concat();
    let last = words.last().map_or("", String::as_str);
    // A working directory, not a password: `?pwd=/Users/me/proj`.
    if last.ends_with("pwd") && value.starts_with(['/', '~']) {
        return false;
    }
    secret_name(name)
        || ["code", "jwt", "ticket", "otp", "totp"].contains(&joined.as_str())
        || ["jwt", "bearer", "otp", "totp"].contains(&last)
        || ["authcode", "accesscode", "mfacode"]
            .iter()
            .any(|end| joined.ends_with(end))
        || ["hmac", "passcode"]
            .iter()
            .any(|part| joined.contains(part))
        || last.ends_with("pwd")
        || last.ends_with("pwds")
        || account_pass(last)
        || HOLDERS.iter().any(|holder| {
            last.strip_suffix("keys")
                .is_some_and(|start| start.ends_with(holder))
        })
        || (last.len() > "tokens".len() && last.ends_with("tokens") && !token_count(value))
}

/// Whether a parameter value is a count rather than a credential: a number,
/// or `null`, `undefined`, `none`, `auto` or `inf` written in its place, or
/// a switch's `true` or `false` (`showtokens=true`).
fn token_count(value: &str) -> bool {
    let number = value.strip_prefix('-').unwrap_or(value);
    let (whole, fraction) = number.split_once('.').unwrap_or((number, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    (digits(whole) && digits(fraction))
        || ["null", "undefined", "none", "auto", "inf", "true", "false"]
            .iter()
            .any(|word| value.eq_ignore_ascii_case(word))
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
            if value.is_empty() || value == REDACTED || !secret_parameter(&name, value) {
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
/// credential flag (`--api-key VALUE`, `-storepass VALUE`), a header flag
/// (`-H VALUE`), a `user:password` flag (`-u VALUE`), OpenSSL's
/// `-passin pass:VALUE` and `sshpass -p VALUE`, the value of `--token=VALUE` and
/// `API_KEY=VALUE`, and any argument that is itself a bearer token, a JWT,
/// a private key, a URL carrying credentials or a `user:password@host`
/// written without a scheme.
#[must_use]
pub fn args(args: &[String]) -> Vec<String> {
    // `sshpass -p PASSWORD`: among sshpass's own options (before the
    // command it runs), `-p` takes the password, not a port.
    let mut sshpass = None;
    if let Some(at) = args
        .iter()
        .position(|arg| arg.rsplit(['/', '\\']).next() == Some("sshpass"))
    {
        let mut index = at + 1;
        while let Some(option) = args.get(index).filter(|arg| arg.starts_with('-')) {
            match option.as_str() {
                "-p" => {
                    sshpass = Some(index + 1);
                    break;
                }
                "-f" | "-d" | "-P" => index += 2,
                _ => index += 1,
            }
        }
    }
    args.iter()
        .enumerate()
        .map(|(index, arg)| {
            let previous = index
                .checked_sub(1)
                .and_then(|previous| args.get(previous))
                .and_then(|previous| flag_name(previous));
            let password = sshpass == Some(index);
            if password || previous.is_some_and(|flag| takes_credential(flag, arg)) {
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
        // OpenSSL's `-passin pass:PASSWORD`; `env:`, `file:` and `fd:` name
        // where it is.
        "pass" | "passin" | "passout" => value.starts_with("pass:"),
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
    match urls_in(arg) {
        Cow::Borrowed(_) => bare_userinfo(arg, REDACTED),
        Cow::Owned(shown) => Cow::Owned(bare_userinfo(&shown, REDACTED).into_owned()),
    }
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
/// credential, URL user info and a `user:password@host` written without a
/// scheme, JWTs, well-known token prefixes and long opaque runs.
/// Over-redaction is fine; this is for log lines and messages, not data.
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
            // OpenSSL's `-passin pass:PASSWORD` (`env:`, `file:` and `fd:`
            // name where it is), and `sshpass -p PASSWORD`, whose `-p` is no port.
            (r#"(^|[^\w-])(-pass(?:in|out)?[ \t]+["']?pass:)([^\s"',;&]+)"#.to_owned(), "${1}${2}[redacted]"),
            (
                r#"(?i:\b(sshpass))((?:[ \t]+-[a-oq-zA-Z]\S*)*[ \t]+-p[ \t]*)("[^"\r\n]*"?|'[^'\r\n]*'?|[^\s"',;&]+)"#.to_owned(),
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
    // Names the `KEYS` rules miss (see `pass_name`), as a pair and as a flag
    // followed by its value. Every one of them holds `pass` or `pw`.
    static NAMED: LazyLock<[Regex; 2]> = LazyLock::new(|| {
        [
            format!(r#"(?i)\b([\w.-]*(?:pass|pw)[\w.-]*)(["']?[ \t]*[=:][ \t]*)({VALUE}|[^\s"',;&]+)"#),
            format!(r#"(?i)(^|[^\w-])(--?([\w.-]*(?:pass|pw)[\w.-]*))[ \t]+({VALUE}|[^\s"',;&-][^\s"',;&]*)"#),
        ]
        .map(|pattern| Regex::new(&pattern).expect("static redaction pattern"))
    });
    // A query or fragment parameter outside a URL with a scheme
    // (`GET /cb?code=…`), named as `url` reads names.
    static PARAMETER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"([?&#])([A-Za-z0-9_.%+-]{1,128})=([^&\s"'#]+)"#)
            .expect("static parameter pattern")
    });
    let mut text = bare_userinfo(&userinfo(text), "[redacted]").into_owned();
    for (pattern, replacement) in RULES.iter() {
        if pattern.is_match(&text) {
            text = pattern.replace_all(&text, *replacement).into_owned();
        }
    }
    let [pairs, flags] = &*NAMED;
    if pairs.is_match(&text) {
        text = pairs
            .replace_all(&text, |found: &regex::Captures<'_>| {
                let value = &found[3];
                let count = value.bytes().all(|byte| byte.is_ascii_digit());
                if pass_name(&found[1])
                    && hides(value)
                    && (!count || account_password_name(&found[1]))
                {
                    format!("{}{}[redacted]", &found[1], &found[2])
                } else {
                    found[0].to_owned()
                }
            })
            .into_owned();
    }
    if flags.is_match(&text) {
        text = flags
            .replace_all(&text, |found: &regex::Captures<'_>| {
                if pass_name(&found[3]) && !switch_name(&found[3]) && hides(&found[4]) {
                    format!("{}{} [redacted]", &found[1], &found[2])
                } else {
                    found[0].to_owned()
                }
            })
            .into_owned();
    }
    if PARAMETER.is_match(&text) {
        text = PARAMETER
            .replace_all(&text, |found: &regex::Captures<'_>| {
                let name = url::form_urlencoded::parse(found[2].as_bytes())
                    .next()
                    .map(|(name, _)| name.into_owned())
                    .unwrap_or_default();
                let value = &found[3];
                if hides(value) && path_parameter(&name, value) {
                    format!("{}{}=[redacted]", &found[1], &found[2])
                } else {
                    found[0].to_owned()
                }
            })
            .into_owned();
    }
    text
}

/// Whether a value found under a credential's name in free text is hidden:
/// anything but what is hidden already (`[redacted]@host` included), a
/// flag's `true` or `false`, and a shell's `$VAR` or `$(…)`, which the text
/// names rather than holds.
fn hides(value: &str) -> bool {
    let unquoted = value.trim_matches(['"', '\'']);
    !(value.starts_with("[redacted]")
        || value.starts_with(REDACTED)
        || unquoted.starts_with('$')
        || unquoted.eq_ignore_ascii_case("true")
        || unquoted.eq_ignore_ascii_case("false"))
}

/// Whether a parameter of a path written without a scheme holds a
/// credential, as [`secret_parameter`] says, except where a path's own
/// names often mean something else: a short whole `code` (an OAuth code
/// is long; `?code=US` and `?code=SUMMER20` are not one) and a key that
/// only sorts, caches or indexes (`sort_key`, `cache_key`, `primary_key`).
fn path_parameter(name: &str, value: &str) -> bool {
    let words = words(name);
    let joined = words.concat();
    if joined == "code" && value.len() < 16 {
        return false;
    }
    if words.len() > 1
        && words.last().is_some_and(|last| last == "key")
        && !secret_name(&words[..words.len() - 1].join("_"))
        && !HOLDERS
            .iter()
            .any(|holder| words[words.len() - 2] == *holder)
    {
        return false;
    }
    secret_parameter(name, value)
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

/// `text` with the user name and password of each `user:password@host`
/// written without a scheme replaced by `mark`, as a proxy or `CONNECT`
/// target (`curl -x user:pw@proxy:3128`, `https_proxy=user:pw@proxy:3128`)
/// or a database address (`root:pw@tcp(db:3306)/app`) reads. As for a URL,
/// everything up to the last `@` goes, so a raw `@` in the password does too.
///
/// The user name is a word with no `:` that starts the text or follows a
/// space, a quote, a bracket, `|`, `` ` ``, `=`, `,`, `;`, `/`, `?`, `#`,
/// `@` or `\`; the password and host run to a space (not a
/// backslash-escaped one), `"`, `/`, `?`, `#` or `\`, so a Windows path is
/// never read as one and the text is read in linear time. A password never
/// starts with `@`, `/`, `:`, `<`, `-` or a space, so `npm:@scope/pkg`,
/// `host:/path`, `crate::path@0x10`, `MAIL FROM:<me@example.com>`,
/// `${USER:-root}@host` and `name: me@example.com` stay, and a word after
/// an `@` is the host of an ssh target (`alex@host:icon@2x.png`), not a
/// user name. See [`hidden_userinfo`] for what else stays.
fn bare_userinfo<'a>(text: &'a str, mark: &str) -> Cow<'a, str> {
    // What may stand before a user name.
    fn head(byte: u8) -> bool {
        byte.is_ascii_whitespace()
            || matches!(
                byte,
                b'"' | b'\''
                    | b'('
                    | b'<'
                    | b'>'
                    | b'['
                    | b']'
                    | b'{'
                    | b'}'
                    | b'|'
                    | b'`'
                    | b'='
                    | b','
                    | b';'
                    | b'/'
                    | b'?'
                    | b'#'
                    | b'@'
                    | b'\\'
            )
    }
    let bytes = text.as_bytes();
    let mut shown = String::new();
    let mut copied = 0;
    let mut at = 0;
    // Whether `at` may start a user name.
    let mut start = true;
    // The end and last `@` of the word a cut password was last looked for in.
    let mut word = None;
    while at < bytes.len() {
        if !start {
            start = head(bytes[at]);
            at += 1;
            continue;
        }
        let user = at;
        let mut colon = at;
        while colon < bytes.len() && bytes[colon] != b':' && !head(bytes[colon]) {
            colon += 1;
        }
        if colon == bytes.len() {
            break;
        }
        if bytes[colon] != b':' {
            // A character that may stand before a user name: the next one
            // may start one.
            at = colon + 1;
            continue;
        }
        let password = colon + 1;
        if bytes.get(password).is_none_or(|&byte| {
            byte.is_ascii_whitespace()
                || matches!(byte, b'"' | b'/' | b'?' | b'#' | b'@' | b':' | b'<' | b'-')
        }) {
            at = password;
            start = false;
            continue;
        }
        let mut end = password;
        while let Some(&byte) = bytes.get(end) {
            match byte {
                b'\\' if bytes.get(end + 1).is_some_and(u8::is_ascii_whitespace) => end += 2,
                b'\\' | b'"' | b'/' | b'?' | b'#' => break,
                _ if byte.is_ascii_whitespace() => break,
                _ => end += 1,
            }
        }
        if end > password
            && matches!(bytes.get(end), Some(b'/' | b'#' | b'?' | b'\\'))
            && !text[password..end].contains('@')
        {
            // `user:p/ss@proxy:3128`: a password holding a raw `/`, `#`, `?`
            // or `\` ends the run early. It runs on to the last `@` of the
            // word when a host and port follow it there.
            end = bare_password_end(bytes, end, &mut word).unwrap_or(end);
        }
        let found = &text[user..end];
        if let Some(last) = found.rfind('@')
            && (user == 0 || bytes[user - 1] != b'@')
            && let Some(hidden) = hidden_userinfo(&found[..last], &found[last + 1..])
        {
            shown.push_str(&text[copied..user]);
            shown.push_str(&found[..hidden]);
            shown.push_str(mark);
            copied = user + last;
        }
        at = end;
        start = false;
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    shown.push_str(&text[copied..]);
    Cow::Owned(shown)
}

/// Where the `user:p/ss@host:port` that a raw `/`, `#`, `?` or `\` cut at
/// `cut` ends: past the host and port after the last `@` of the word; `None`
/// unless a host and a port follow that `@`. `word` keeps the end and last
/// `@` of the word last read, which every cut inside it shares, so a long
/// word is read once.
fn bare_password_end(
    bytes: &[u8],
    cut: usize,
    word: &mut Option<(usize, Option<usize>)>,
) -> Option<usize> {
    let (end, last) = match *word {
        Some((end, last)) if cut < end => (end, last),
        _ => {
            let mut end = cut;
            let mut last = None;
            while let Some(&byte) = bytes.get(end) {
                match byte {
                    b'\\' if bytes.get(end + 1).is_some_and(u8::is_ascii_whitespace) => end += 2,
                    _ if byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\'' | b'`') => break,
                    b'@' => {
                        last = Some(end);
                        end += 1;
                    }
                    _ => end += 1,
                }
            }
            let end = end.min(bytes.len());
            *word = Some((end, last));
            (end, last)
        }
    };
    let at = last.filter(|&at| at > cut)?;
    let host = at
        + 1
        + bytes[at + 1..end]
            .iter()
            .position(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')))?;
    if host == at + 1 || bytes[host] != b':' {
        return None;
    }
    let port = host
        + 1
        + bytes[host + 1..end]
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .unwrap_or(end - host - 1);
    (port > host + 1 && (port == end || bytes[port] == b'/')).then_some(port)
}

/// For `info@after`, a bare `user:password@host` that [`bare_userinfo`]
/// found: where in `info` the hidden part starts (after an address scheme
/// such as `mailto:`), or `None` when it holds no credential.
///
/// These stay: what pins an image or a package rather than names a host
/// (`alpine:3.19@sha256:…`, `node:20@v1.2.3`); an `npm:` or `jsr:` spec
/// (`npm:pkg@1.0`); an IPv6 address (`2001:db8::1@eth0`); a time
/// (`10:15@home`, `3:30pm@office`, `2026-10-24T08:00:00Z@db1`) or a ratio
/// (`16:9@2x`); an address after `mailto:`, `sip:`, `xmpp:`, `acct:`, a
/// cloud member kind (`serviceAccount:`, `group:`, `domain:`, `principal:`)
/// or a mail header or search field (`from:`, `to:`, `cc:`, `bcc:`,
/// `email:`), unless a second `:` says it holds a password; and an email
/// address after any label (`user:alex@example.com`, `owner:a@b.co`), or any
/// address after a capitalised field name as Go prints a struct
/// (`Email:alex@localhost`). An address there is a local part a mailbox may
/// have, then a host without a port: emails are common and passwords there
/// are rare, so `user:pw@proxy:3128` and `root:pw@tcp(db)` still go.
fn hidden_userinfo(info: &str, after: &str) -> Option<usize> {
    // An image digest (`sha256:` with a letter, which no port has, or a
    // long hex run, also after the free-text masker hid it) or a version.
    static PINNED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)^(?:(?:sha\d+|md5):[\da-f]*[a-f][\da-f]*|[a-z\d]+(?:[+._-][a-z\d]+)*:(?:[\da-f]{32,}|\[redacted\]|<redacted>)|v?\d+\.\d+\.\d+(?:[-+][\w.-]*)?)",
        )
        .expect("static pinned pattern")
    });
    // A time: `10:15`, `10:15:30.5`, `3:30pm`, `2026-10-24T08:00:00Z`.
    static TIME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:\d{4}-\d{2}-\d{2}t)?\d{1,2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:z|[ap]m|[+-]\d{2}:?\d{2})?$")
            .expect("static time pattern")
    });
    let pinned = PINNED.find(after).is_some_and(|found| {
        !after[found.end()..].starts_with(|next: char| {
            next.is_alphanumeric() || matches!(next, '_' | '.' | ':' | '-')
        })
    });
    let (scheme, rest) = info.split_once(':')?;
    let numeric = |part: &str, more: &[u8]| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || more.contains(&byte))
    };
    if pinned
        || info.contains("::")
        || TIME.is_match(info)
        || (numeric(scheme, b"") && numeric(rest, b":."))
        || (["npm", "jsr"]
            .iter()
            .any(|spec| scheme.eq_ignore_ascii_case(spec))
            && !rest.is_empty()
            && !rest.contains([':', '@']))
    {
        return None;
    }
    if [
        "mailto",
        "sip",
        "sips",
        "xmpp",
        "acct",
        "serviceaccount",
        "group",
        "domain",
        "principal",
        "from",
        "to",
        "cc",
        "bcc",
        "email",
    ]
    .iter()
    .any(|address| scheme.eq_ignore_ascii_case(address))
    {
        return rest.contains(':').then_some(scheme.len() + 1);
    }
    // A mailbox's local part, then a host with no port after it.
    let mailbox = !rest.is_empty()
        && rest.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
        });
    let host = after
        .find(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
        })
        .map_or(after, |end| &after[..end]);
    let port = after[host.len()..].starts_with(':');
    if mailbox && !host.is_empty() && !port {
        let domain = host.rsplit_once('.').is_some_and(|(name, top)| {
            !name.is_empty() && top.len() >= 2 && top.bytes().all(|byte| byte.is_ascii_alphabetic())
        });
        let field = scheme.starts_with(|character: char| character.is_ascii_uppercase())
            && scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'));
        if domain || field {
            return None;
        }
    }
    Some(0)
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
    fn free_text_hides_passcodes_passphrases_and_a_pass_pw_or_pwd_after_another_word() {
        for (input, output) in [
            ("export DB_PASS=PRIVATE", "export DB_PASS=[redacted]"),
            ("dbPass: PRIVATE next", "dbPass: [redacted] next"),
            (
                "MYSQL_PWD=PRIVATE mysql -u root app",
                "MYSQL_PWD=[redacted] mysql -u root app",
            ),
            (
                "SSHPASS=PRIVATE sshpass -e ssh host",
                "SSHPASS=[redacted] sshpass -e ssh host",
            ),
            ("DBPASS='PRIVATE PART' ./run", "DBPASS=[redacted] ./run"),
            ("x-pw: PRIVATE", "x-pw: [redacted]"),
            ("passphrase=PRIVATE", "passphrase=[redacted]"),
            ("PASS_PHRASE=PRIVATE", "PASS_PHRASE=[redacted]"),
            (
                "gpg --batch --passphrase 'PRIVATE PART' -d f.gpg",
                "gpg --batch --passphrase [redacted] -d f.gpg",
            ),
            ("passcode: 1234", "passcode: [redacted]"),
            ("pass_code=1234&next=1", "pass_code=[redacted]&next=1"),
            (
                r#"{"devicePasscode":"PRIVATE","ok":1}"#,
                r#"{"devicePasscode":[redacted],"ok":1}"#,
            ),
            (
                "tool --db-pass PRIVATE --verbose",
                "tool --db-pass [redacted] --verbose",
            ),
        ] {
            assert_eq!(text(input), output, "{input}");
            assert_eq!(text(output), output, "{output}");
        }
        for kept in [
            "tests pass: 3, fail: 0",
            "PWD=/Users/me/project OLDPWD=/tmp",
            "pass=ok pw: x",
            "bypass=1 compass: north trespass=no",
            "bypass_code=ABC",
            "passenger: Alex",
            "hasPassphrase=false passcode: True",
            "passcode_length=6 passcodeLength: 6",
            "--db-pass true",
            "npm run pass:all",
            "pwsh -NoProfile -Command Get-Date",
            // Test counts and switches named after passing.
            "tests_pass=12 tests_fail=0 num_pass: 7, num_fail: 1 lint_pass: true",
            "--skip-pass build --ask-pass playbook.yml",
            "DB_PASS=[redacted]@h DB_PASS=$DB_PASSWORD",
        ] {
            assert_eq!(text(kept), kept);
        }
        // An account's password is hidden when it is a number too.
        assert_eq!(
            text("DB_PASS=123456 MYSQL_ROOT_PWD=4242 --db-pass 99"),
            "DB_PASS=[redacted] MYSQL_ROOT_PWD=[redacted] --db-pass [redacted]"
        );
        assert_eq!(
            args(&["--passcode", "1234", "PASS_CODE=5678"].map(str::to_owned)),
            ["--passcode", REDACTED, "PASS_CODE=<redacted>"]
        );
        assert!(!secret_name("bypass_code"));
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
    fn free_text_hides_credential_parameters_of_a_path_without_a_scheme() {
        for (input, output) in [
            (
                "GET /cb?code=PRIVATE0123456789abc&state=s failed: 400",
                "GET /cb?code=[redacted]&state=s failed: 400",
            ),
            (
                "/x?api_key=PRIVATE&key=PRIVATE",
                "/x?api_key=[redacted]&key=[redacted]",
            ),
            (
                "GET /p?n=1&pwd=PRIVATE returned 401",
                "GET /p?n=1&pwd=[redacted] returned 401",
            ),
            ("/login?x-otp=123456", "/login?x-otp=[redacted]"),
            (
                "/cb#dbpass=PRIVATE&userpw=PRIVATE&ticket=PRIVATE",
                "/cb#dbpass=[redacted]&userpw=[redacted]&ticket=[redacted]",
            ),
            ("/x?api%5Fkey=PRIVATE", "/x?api%5Fkey=[redacted]"),
        ] {
            assert_eq!(text(input), output, "{input}");
            assert_eq!(text(output), output, "{output}");
        }
        for kept in [
            "GET /search?q=hello&page=2 200",
            "/x?keyword=k&bypass=1&compass=2#install",
            "a=1&b=2&state=s",
            "/x?code=&next=1",
            // A short code names a country or an offer, a switch is a bit, a
            // shell names a value, and these keys sort, cache or index.
            "GET /api/countries?code=US&name=United /products?code=SUMMER20&lang=en",
            "?pass=false&key=true&passcode=TRUE",
            "GET /v1/items?sort_key=name&cache_key=abc&primary_key=id",
            "cd /tmp&&pwd=$(pwd) echo",
            "GET /v1/files?pwd=/Users/me/proj 200",
        ] {
            assert_eq!(text(kept), kept);
        }
    }

    #[test]
    fn free_text_hides_a_user_and_password_written_without_a_scheme() {
        let sha = "ab12".repeat(16);
        for (input, output) in [
            (
                "curl -x user:PRIVATE@proxy.fixture.test:3128 https://x.test",
                "curl -x [redacted]@proxy.fixture.test:3128 https://x.test",
            ),
            (
                "https_proxy=user:PRIVATE@proxy.fixture.test:3128",
                "https_proxy=[redacted]@proxy.fixture.test:3128",
            ),
            (
                "dsn root:PRIVATE@tcp(127.0.0.1:3306)/app",
                "dsn [redacted]@tcp(127.0.0.1:3306)/app",
            ),
            (
                "CONNECT user:PRIVATE@h.fixture.test:443 HTTP/1.1",
                "CONNECT [redacted]@h.fixture.test:443 HTTP/1.1",
            ),
            (
                ":PRIVATE@h.fixture.test:443",
                "[redacted]@h.fixture.test:443",
            ),
            ("user:p@ss@h.fixture.test", "[redacted]@h.fixture.test"),
            ("user:PRIVATE@[::1]:443", "[redacted]@[::1]:443"),
            (
                r"host=user:pa\ ss@h.fixture.test:443",
                "host=[redacted]@h.fixture.test:443",
            ),
            (
                "getaddrinfo ENOTFOUND user:PRIVATE@h.fixture.test:443",
                "getaddrinfo ENOTFOUND [redacted]@h.fixture.test:443",
            ),
            (
                "proxy answered 407 for 'user:PRIVATE@h.fixture.test:443'",
                "proxy answered 407 for '[redacted]@h.fixture.test:443'",
            ),
            (
                r#"{"proxy":"user:PRIVATE@h.fixture.test:3128"}"#,
                r#"{"proxy":"[redacted]@h.fixture.test:3128"}"#,
            ),
            (
                "->user:pw@h.fixture.test:443",
                "->[redacted]@h.fixture.test:443",
            ),
            ("[net]user:pw@h:443", "[net][redacted]@h:443"),
            ("a|user:pass@host", "a|[redacted]@host"),
            ("a/user:pw@h:443", "a/[redacted]@h:443"),
            (
                r"CORP\alex:PRIVATE@proxy:8080",
                r"CORP\[redacted]@proxy:8080",
            ),
            // A spec or an address that says it holds a password.
            (
                "npm:user:PRIVATE@h.fixture.test",
                "[redacted]@h.fixture.test",
            ),
            ("npm:a@b@h.fixture.test", "[redacted]@h.fixture.test"),
            (
                "mailto:user:PRIVATE@h.fixture.test",
                "mailto:[redacted]@h.fixture.test",
            ),
            // A host named like a digest algorithm or with a version-like
            // address and a port is still a host.
            (
                "CONNECT user:PRIVATE@sha256:8080",
                "CONNECT [redacted]@sha256:8080",
            ),
            ("user:PRIVATE@10.0.0.1:3128", "[redacted]@10.0.0.1:3128"),
            (
                "1000:PRIVATE@h.fixture.test:80",
                "[redacted]@h.fixture.test:80",
            ),
            // A local part a mailbox cannot have, or a host with no name.
            ("db=root:PRIVATE@/app", "db=[redacted]@/app"),
            (
                "root:PRIVATE@unix(/tmp/mysql.sock)/app",
                "[redacted]@unix(/tmp/mysql.sock)/app",
            ),
            (
                "user:PRIVATE@[2001:db8::1]:3128",
                "[redacted]@[2001:db8::1]:3128",
            ),
            (
                "Admin:PRIVATE@db.fixture.test:5432",
                "[redacted]@db.fixture.test:5432",
            ),
        ] {
            assert_eq!(text(input), output, "{input}");
            assert_eq!(text(output), output, "{output}");
        }
        for kept in [
            "git@github.com:org/repo.git",
            "scp build.tgz alex@h.fixture.test:/tmp/",
            "ssh -J alex@jump.fixture.test:2222 alex@h.fixture.test",
            "rsync -a alex@h.fixture.test::module/ out/",
            "npm:some-ext@1.2.0",
            "npm:@scope/ext@1.2.0-beta.1",
            "spec=npm:some-ext@latest jsr:x@^2.0.0",
            "mailto:someone@h.fixture.test",
            "sip:alice@h.fixture.test xmpp:bob@h.fixture.test",
            "img:tag@sha256:abc",
            "node:20@v1.2.3 ruby:3.3@3.3.0",
            "Signed-off-by: Alex <alex@h.fixture.test>",
            "From: alex@h.fixture.test",
            r"C:\proj\node_modules\@types\node\index.d.ts",
            r"C:\proj\node_modules\.pnpm\react@18.2.0\node_modules\react",
            "C:/proj/node_modules/@babel/core/lib/index.js",
            "listening on 127.0.0.1:4096, [::1]:4096",
            "tokio::runtime::task@0x10",
            "refs/heads/main@{1} HEAD@{2026-10-24 08:00:00}",
            r#"{"name":"a:b","email":"someone@h.fixture.test"}"#,
            r#"{\"url\":\"http://localhost:3000\",\"email\":\"me@example.com\"}"#,
            "at async run (node:internal/process/task_queues:95:5)",
            "go: example.com/m@v1.0.0: missing go.sum entry",
            "meet 10:15@home, 10:15:30@main, icon 16:9@2x",
            // Addresses after a label, which an email is far more often than a
            // password: cloud members, mail headers and search fields, Go's
            // struct printing, and a name a DNS lookup or a log line quotes.
            "gcloud projects add-iam-policy-binding p --member=user:alex@example.com --role=roles/viewer",
            "--iam-account=serviceAccount:ci@proj.iam.gserviceaccount.com",
            r#""members": ["user:alex@example.com", "group:devs@example.com", "domain:example.com"]"#,
            "principal:alex@example.com",
            "> MAIL FROM:<alex@example.com> SIZE=1024",
            "> RCPT TO:<bob@example.com>",
            "search from:alice@example.com to:bob@example.com cc:c@example.com bcc:d@example.com",
            "email:alex@example.com,name:Alex",
            "user.email:alex@example.com AND status:active",
            "To:alex@example.com",
            "login ok for account:alex@example.com",
            r#"level=info msg="created user" user={ID:42 Email:alex@example.com Role:admin}"#,
            "&{Name:alex Email:alex@example.com Admin:false}",
            "Owner:alex@localhost",
            "getaddrinfo ENOTFOUND user:alex@h.fixture.test",
            "scp host:db@2026.tgz .",
            // Shell defaults, ssh targets and protocol specs.
            "ssh ${DEPLOY_USER:-deploy}@${HOST} uptime",
            "scp build.tgz ${USER:-root}@10.0.0.5:/tmp/",
            "scp alex@build.fixture.test:icon@2x.png .",
            r#"resolution: "pkg@patch:pkg@npm%3A1.0.0#~/.yarn/patches/pkg.patch""#,
            // Times and IPv6 addresses.
            "backup finished 2026-10-24T08:00:00Z@db1 in 3s",
            "call me at 3:30pm@office, time 12:30:45.123@worker-3",
            "2001:db8::1@eth0 [2001:db8::1]:443@x fe80::1%en0@host",
        ] {
            assert_eq!(text(kept), kept);
        }
        // A digest's hex is a long run the free-text masker hides anyway.
        assert_eq!(
            text(&format!("pulled alpine:3.19@sha256:{sha}")),
            "pulled alpine:3.19@sha256:[redacted]"
        );
        assert_eq!(
            text("pulled alpine:3.19@sha256:[redacted]"),
            "pulled alpine:3.19@sha256:[redacted]"
        );
        // MCP arguments in those forms stay as written, so they stay editable.
        let kept = [
            "--member=user:alex@example.com",
            "serviceAccount:ci@proj.iam.gserviceaccount.com",
            "alex@build.fixture.test:icon@2x.png",
            "${USER:-root}@10.0.0.5:/tmp/",
            "Email:alex@example.com",
            "2026-10-24T08:00:00Z@db1",
        ]
        .map(str::to_owned);
        assert_eq!(args(&kept), kept);
        assert_eq!(
            args(
                &[
                    "--proxy",
                    "user:PRIVATE@proxy.fixture.test:3128",
                    "HTTPS_PROXY=user:PRIVATE@proxy.fixture.test:3128",
                    "git@github.com:org/repo.git",
                    "npm:some-ext@1.2.0",
                ]
                .map(str::to_owned)
            ),
            [
                "--proxy",
                "<redacted>@proxy.fixture.test:3128",
                "HTTPS_PROXY=<redacted>@proxy.fixture.test:3128",
                "git@github.com:org/repo.git",
                "npm:some-ext@1.2.0",
            ]
        );
    }

    #[test]
    fn free_text_hides_openssl_keytool_and_sshpass_passwords_and_a_bare_one_holding_a_slash() {
        for (input, output) in [
            (
                "openssl pkcs12 -export -in c.pem -out c.p12 -passout pass:PRIVATE",
                "openssl pkcs12 -export -in c.pem -out c.p12 -passout pass:[redacted]",
            ),
            (
                "openssl rsa -in k.pem -passin pass:PRIVATE -out plain.pem",
                "openssl rsa -in k.pem -passin pass:[redacted] -out plain.pem",
            ),
            (
                "openssl enc -aes256 -pass pass:PRIVATE -in f",
                "openssl enc -aes256 -pass pass:[redacted] -in f",
            ),
            (
                "keytool -genkeypair -keystore ks.jks -storepass PRIVATE -keypass PRIVATE",
                "keytool -genkeypair -keystore ks.jks -storepass [redacted] -keypass [redacted]",
            ),
            (
                "jarsigner -keystore ks.jks -storepass PRIVATE app.apk alias",
                "jarsigner -keystore ks.jks -storepass [redacted] app.apk alias",
            ),
            (
                "sshpass -p PRIVATE ssh alex@host",
                "sshpass -p [redacted] ssh alex@host",
            ),
            (
                "sshpass -v -p 'PRIVATE PART' ssh alex@host",
                "sshpass -v -p [redacted] ssh alex@host",
            ),
            (
                "proxy user:p/PRIVATE@proxy.fixture.test:3128",
                "proxy [redacted]@proxy.fixture.test:3128",
            ),
            (
                "proxy user:p#PRIVATE@proxy.fixture.test:3128/x",
                "proxy [redacted]@proxy.fixture.test:3128/x",
            ),
            (
                r"proxy user:pa\PRIVATE@proxy.fixture.test:3128",
                "proxy [redacted]@proxy.fixture.test:3128",
            ),
            (
                "proxy user:p?PRIVATE@10.0.0.1:8080 refused",
                "proxy [redacted]@10.0.0.1:8080 refused",
            ),
        ] {
            assert_eq!(text(input), output, "{input}");
            assert_eq!(text(output), output, "{output}");
        }
        for kept in [
            "openssl rsa -in k.pem -passin env:KEY_PASS -passout file:pw.txt",
            "ssh -p 2222 alex@host && scp -P 2222 f alex@host:/tmp",
            "node:internal/process/task_queues:95:5",
            "image=registry.fixture.test:5000/app:1.0@sha256:abc123",
            r"C:\proj\x@y.fixture.test:80",
            "see a:b/c@d and x:y#z@w",
        ] {
            assert_eq!(text(kept), kept);
        }
        assert_eq!(
            args(
                &[
                    "-passin",
                    "pass:PRIVATE",
                    "-passout",
                    "env:KEY_PASS",
                    "-storepass",
                    "PRIVATE",
                    "--proxy",
                    "user:p/PRIVATE@proxy.fixture.test:3128",
                ]
                .map(str::to_owned)
            ),
            [
                "-passin",
                REDACTED,
                "-passout",
                "env:KEY_PASS",
                "-storepass",
                REDACTED,
                "--proxy",
                "<redacted>@proxy.fixture.test:3128",
            ]
        );
        assert_eq!(
            args(&["sshpass", "-p", "PRIVATE", "ssh", "-p", "22", "h"].map(str::to_owned)),
            ["sshpass", "-p", REDACTED, "ssh", "-p", "22", "h"]
        );
        assert_eq!(
            args(&["ssh", "-p", "22", "h"].map(str::to_owned)),
            ["ssh", "-p", "22", "h"]
        );
    }

    #[test]
    fn free_text_reads_long_runs_of_bare_user_names_and_passwords_quickly() {
        let started = std::time::Instant::now();
        for run in [
            "a@".repeat(100_000),
            ",a:".repeat(100_000),
            "user:pass@".repeat(20_000),
            format!("a:{}@x", "\\ ".repeat(100_000)),
            ":".repeat(200_000),
            "a:b:".repeat(100_000),
            "x(".repeat(100_000),
            "a:b/".repeat(100_000),
            format!("a:b/{}@h:1", "c/".repeat(100_000)),
        ] {
            let _ = bare_userinfo(&run, "[redacted]");
            let _ = text(&run);
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
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
    fn a_url_hides_parameters_whose_names_glue_a_credential_word_to_another() {
        for name in [
            "pwd",
            "PWD",
            "pwds",
            "userpwd",
            "adminpwd",
            "oldpwd",
            "userpwds",
            "userpass",
            "dbpass",
            "adminpass",
            "superuserpass",
            "rootpw",
            "userpw",
            "apitokens",
            "authtokens",
            "apikeys",
            "sshkeys",
            "x-otp",
            "x-jwt",
            "x-bearer",
        ] {
            // A number under a glued `tokens` name counts them.
            let values: &[&str] = if name.ends_with("tokens") {
                &["PRIVATE"]
            } else {
                &["PRIVATE", "1234"]
            };
            for value in values {
                assert_eq!(
                    url(&format!("https://h.fixture.test/x?a=1&{name}={value}&b=2")),
                    format!("https://h.fixture.test/x?a=1&{name}=<redacted>&b=2"),
                    "{name}={value}"
                );
                assert_eq!(
                    urls_in(&format!(
                        "GET https://h.fixture.test/cb#{name}={value}&state=s failed: 401"
                    )),
                    format!("GET https://h.fixture.test/cb#{name}=<redacted>&state=s failed: 401"),
                    "{name}={value}"
                );
            }
        }
        // A glued token count, a glued `pass` that opens nothing, and words
        // that only hold the letters.
        let kept = "https://h.fixture.test/?bypass=1&compass=2&trespass=3&surpass=4&passenger=5\
                    &pattern=a&show_pw_hint=5&keyword=k&monkeys=6&maxtokens=4096&prompttokens=null\
                    &maxtokens=auto&tokenizer=bpe&sid=4&state=s&showtokens=true\
                    &support_ticket=JIRA-1&pwd=/Users/me/proj&oldpwd=~/src";
        assert!(matches!(url(kept), Cow::Borrowed(_)), "{}", url(kept));
        assert_eq!(
            url("https://h.fixture.test/?apitokens=4096x"),
            "https://h.fixture.test/?apitokens=<redacted>"
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
