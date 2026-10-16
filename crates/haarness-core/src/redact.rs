//! Credentials hidden from what Haarness shows of a configuration: the MCP
//! settings that `haarness mcp get`, Settings and the model's `mcp_manage`
//! tool display, and the connection errors of a server. Only the display
//! changes; the configuration in use keeps its values.

use std::borrow::Cow;

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
}
