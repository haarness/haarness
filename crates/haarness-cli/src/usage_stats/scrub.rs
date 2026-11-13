//! What usage statistics keep of an error's words (docs/USAGE-STATISTICS.md):
//! Haarness's own wording and the provider's, with what could name the
//! person, their computer or their work taken out first.
//!
//! Credentials go as Haarness's logs lose them ([`haarness_core::redact`]);
//! then email addresses, paths, file names, private web addresses and hosts,
//! IP addresses other than this computer's, long numbers (account and project
//! numbers) and the names a person gave their own providers and models.
//! Over-scrubbing is fine: the words are for telling failures apart, not for
//! reading back what happened.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::{Captures, Regex};

/// Domains of the services Haarness talks to: their addresses say which
/// service failed and name nobody. Any other host is the person's own.
const PUBLIC_DOMAINS: &[&str] = &[
    "amazonaws.com",
    "anthropic.com",
    "azure.com",
    "bigmodel.cn",
    "cerebras.ai",
    "chatgpt.com",
    "deepinfra.com",
    "deepseek.com",
    "fireworks.ai",
    "github.com",
    "githubcopilot.com",
    "githubusercontent.com",
    "google.com",
    "google.dev",
    "googleapis.com",
    "groq.com",
    "haarness.ai",
    "huggingface.co",
    "mistral.ai",
    "moonshot.ai",
    "moonshot.cn",
    "ollama.com",
    "openai.com",
    "openrouter.ai",
    "together.ai",
    "together.xyz",
    "x.ai",
    "z.ai",
];

/// The first segments of a provider API's path: `/v1/chat/completions` says
/// which call failed and is nobody's file.
const API_SEGMENTS: &[&str] = &[
    "api",
    "chat",
    "compatible-mode",
    "models",
    "openai",
    "responses",
    "v1",
    "v1beta",
    "v2",
    "v3",
];

fn public_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    local_host(&host)
        || PUBLIC_DOMAINS
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn local_host(host: &str) -> bool {
    matches!(
        host,
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "[::1]" | "host.docker.internal"
    )
}

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+").expect("email pattern")
});

/// A web address: its scheme, host and port, then the rest.
static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b([a-z][a-z0-9+.-]*)://([^/\s"'<>()\[\]{}?#:]+|\[[0-9a-f:]+\])(:\d+)?([^\s"'<>]*)"#,
    )
    .expect("url pattern")
});

/// A Windows path (`C:\…`, `C:/…`) or a network share (`\\server\…`), up to a
/// blank or a quote.
static WINDOWS_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:\b[A-Za-z]:[\\/]|\\\\[^\\\s"']+\\)[^\s"'<>|*?;,]*"#)
        .expect("windows path pattern")
});

/// A path from the root or the home folder with at least two parts, or one
/// relative to a folder that ends at a file name (`src/main.rs`).
static UNIX_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|[\s"'`(\[=:,])((?:~|\$HOME)?/[^\s"'`<>()\[\],;:/]+(?:/[^\s"'`<>()\[\],;:]*)+|~/[^\s"'`<>()\[\],;:]*|[A-Za-z0-9_.@-]+(?:[/\\][A-Za-z0-9_.@ -]*[A-Za-z0-9_@-])*[/\\][A-Za-z0-9_@-][A-Za-z0-9_.@-]*\.[A-Za-z][A-Za-z0-9]{0,7})"#)
        .expect("unix path pattern")
});

/// A name with dots in it: a host (`gw.corp.example.com`) or a file name
/// (`main.rs`, `.env`). Never right after `]` or `)`, where the dots are a
/// field path's (`tools[0].function_declarations[1].parameters`).
static DOTTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^A-Za-z0-9_.@<\])-])(\.[A-Za-z][A-Za-z0-9_-]*(?:\.[A-Za-z0-9_-]+)*|[A-Za-z0-9_][A-Za-z0-9_-]*(?:\.[A-Za-z0-9_-]+)+)").expect("dotted pattern")
});

static IPV4: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").expect("ipv4 pattern"));

/// Runs of nine digits or more: account, project and phone numbers. Token
/// counts and limits stay (a context window is at most eight digits).
static LONG_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{9,}").expect("number pattern"));

/// `words` with what could name the person, their computer or their work
/// taken out, `private` names (a provider or model the person named) among
/// them, on one line of at most `max` characters. Empty when nothing is
/// left.
pub(super) fn words(words: &str, private: &[&str], max: usize) -> String {
    // Addresses first: a private one goes whole, a public one loses its
    // query, so nothing of either is left for the credential pass to mark.
    let text = urls(words);
    let text = haarness_core::redact::text(&haarness_core::redact::urls_in(&text));
    let text = names(&text, private);
    let text = EMAIL.replace_all(&text, "<email>");
    let text = WINDOWS_PATH.replace_all(&text, "<path>");
    let text = unix_paths(&text);
    let text = dotted(&text);
    let text = IPV4.replace_all(&text, |found: &Captures| {
        if local_host(&found[0]) {
            found[0].to_owned()
        } else {
            "<ip>".to_owned()
        }
    });
    let text = LONG_NUMBER.replace_all(&text, "<number>");
    let line: Vec<&str> = text
        .split(|character: char| character.is_whitespace() || character.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    haarness_core::redact::clip(&line.join(" "), max)
}

/// Every one of `private` (three characters or more) as `<name>`, whatever
/// its case.
fn names(text: &str, private: &[&str]) -> String {
    let mut private: Vec<&str> = private
        .iter()
        .copied()
        .filter(|name| name.chars().count() >= 3)
        .collect();
    // Longest first, so a model named after its provider goes whole.
    private.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let mut text = text.to_owned();
    for name in private {
        let pattern = format!("(?i){}", regex::escape(name));
        if let Ok(pattern) = Regex::new(&pattern) {
            text = pattern.replace_all(&text, "<name>").into_owned();
        }
    }
    text
}

/// A web address as its scheme and host when the host is a public service's
/// or this computer's, `<url>` otherwise. The path goes too unless it is a
/// provider API's.
fn urls(text: &str) -> Cow<'_, str> {
    URL.replace_all(text, |found: &Captures| {
        let scheme = &found[1];
        let host = &found[2];
        let rest = found.get(4).map_or("", |rest| rest.as_str());
        // Punctuation that ends the sentence around the address stays.
        let trimmed = rest.trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
        let after = &rest[trimmed.len()..];
        if !public_host(host) {
            return format!("<url>{after}");
        }
        let port = found.get(3).map_or("", |port| port.as_str());
        let path = trimmed.split(['?', '#']).next().unwrap_or_default();
        let api = path
            .trim_start_matches('/')
            .split('/')
            .next()
            .is_some_and(|first| API_SEGMENTS.contains(&first.to_ascii_lowercase().as_str()));
        if api && path.split('/').all(|part| !part.contains('.')) {
            format!("{scheme}://{host}{port}{path}{after}")
        } else {
            format!("{scheme}://{host}{port}{after}")
        }
    })
}

fn unix_paths(text: &str) -> Cow<'_, str> {
    UNIX_PATH.replace_all(text, |found: &Captures| {
        let whole = &found[0];
        let path = &found[1];
        let before = &whole[..whole.len() - path.len()];
        let first = path
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        // An API path a provider named (`POST /v1/messages`) is kept; so is a
        // model id with its owner (`qwen/qwen3-coder`), which has no file
        // name at its end.
        if path.starts_with('/') && API_SEGMENTS.contains(&first.as_str()) {
            return whole.to_owned();
        }
        format!("{before}<path>")
    })
}

/// A dotted name as itself when it is a public service's host or a version
/// or model number (`0.1.21`, `gemini-2.5-flash`); as `<host>` when it has a
/// top-level domain, `<file>` otherwise.
fn dotted(text: &str) -> Cow<'_, str> {
    DOTTED.replace_all(text, |found: &Captures| {
        let whole = &found[0];
        let name = &found[1];
        let before = &whole[..whole.len() - name.len()];
        let last = name.rsplit('.').next().unwrap_or_default();
        // Versions, decimals and model numbers: what follows the last dot
        // starts with a digit.
        if last.is_empty() || last.starts_with(|c: char| c.is_ascii_digit()) {
            return whole.to_owned();
        }
        // An abbreviation's dots (`e.g`, `i.e`).
        if name.split('.').all(|part| part.chars().count() <= 1) {
            return whole.to_owned();
        }
        if public_host(name) {
            return whole.to_owned();
        }
        let parts = name.split('.').filter(|part| !part.is_empty()).count();
        let domain = last.len() >= 2 && last.chars().all(|c| c.is_ascii_alphabetic());
        if parts >= 3 && domain {
            format!("{before}<host>")
        } else {
            format!("{before}<file>")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrub(text: &str) -> String {
        words(text, &[], 500)
    }

    #[test]
    fn provider_words_stay_and_the_person_s_go() {
        assert_eq!(
            scrub(
                "This API key has used up today's free allowance for Gemini 3 Flash on Gemini, which comes back at midnight Pacific time, in about 9 hours."
            ),
            "This API key has used up today's free allowance for Gemini 3 Flash on Gemini, which comes back at midnight Pacific time, in about 9 hours."
        );
        assert_eq!(
            scrub(
                "Could not reach Gemini at generativelanguage.googleapis.com. Check the network connection and provider endpoint, then retry."
            ),
            "Could not reach Gemini at generativelanguage.googleapis.com. Check the network connection and provider endpoint, then retry."
        );
        assert_eq!(
            scrub(
                "Provider error: HTTP 400 Bad Request: This endpoint's maximum context length is 163840 tokens. However, you requested about 170001 tokens (gemini-2.5-flash, v0.1.21)."
            ),
            "Provider error: HTTP 400 Bad Request: This endpoint's maximum context length is 163840 tokens. However, you requested about 170001 tokens (gemini-2.5-flash, v0.1.21)."
        );
    }

    #[test]
    fn paths_and_file_names_go() {
        assert_eq!(
            scrub("Provider error: HTTP 500: SECRET STACK at /Users/sam/project/main.rs:12"),
            "Provider error: HTTP 500: SECRET STACK at <path>:12"
        );
        let spaced =
            scrub(r"old was not found in C:\Users\Sam Rivera\work\app.py; nothing was changed");
        for gone in ["Sam", "Rivera", "work", "app.py"] {
            assert!(!spaced.contains(gone), "{gone} left in {spaced}");
        }
        assert!(spaced.ends_with("; nothing was changed"), "{spaced}");
        let windows = scrub(r"old was not found in C:\Users\sam\work\app.py; nothing was changed");
        assert_eq!(windows, "old was not found in <path>; nothing was changed");
        assert_eq!(
            scrub("old was not found in src/billing/invoice.ts; nothing was changed"),
            "old was not found in <path>; nothing was changed"
        );
        assert_eq!(
            scrub("old was not found in main.c; nothing was changed. Read the file again"),
            "old was not found in <file>; nothing was changed. Read the file again"
        );
        assert_eq!(scrub("cannot open ~/.ssh/config"), "cannot open <path>");
        assert_eq!(scrub("could not read .env"), "could not read <file>");
        assert_eq!(
            scrub("ENOENT: no such file '/home/sam/a'"),
            "ENOENT: no such file '<path>'"
        );
    }

    #[test]
    fn private_addresses_go_and_public_ones_stay() {
        assert_eq!(
            scrub("Acme has no chat API at https://llm.corp.example.com:8443/v1/chat?team=red"),
            "Acme has no chat API at <url>"
        );
        assert_eq!(
            scrub("Could not reach Ollama at http://localhost:11434/api/chat"),
            "Could not reach Ollama at http://localhost:11434/api/chat"
        );
        assert_eq!(
            scrub("POST https://openrouter.ai/api/v1/chat/completions?key=abc failed"),
            "POST https://openrouter.ai/api/v1/chat/completions failed"
        );
        assert_eq!(
            scrub("POST /v1/messages answered 529"),
            "POST /v1/messages answered 529"
        );
        assert_eq!(
            scrub("connect to gw.corp.example.com refused; from 10.1.2.3 to 127.0.0.1"),
            "connect to <host> refused; from <ip> to 127.0.0.1"
        );
        assert_eq!(
            scrub("No endpoints found for qwen/qwen3-coder."),
            "No endpoints found for qwen/qwen3-coder."
        );
        assert_eq!(
            scrub(
                "Unknown name \"additionalProperties\" at 'tools[0].function_declarations[1].parameters': Cannot find field. See https://ai.google.dev/gemini-api/docs/thought-signatures"
            ),
            "Unknown name \"additionalProperties\" at 'tools[0].function_declarations[1].parameters': Cannot find field. See https://ai.google.dev"
        );
    }

    #[test]
    fn people_accounts_and_secrets_go() {
        let scrubbed = scrub(
            "Account sam.rivera@example.com (project 123456789012) refused key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 with Authorization: Bearer abcdefghijklmnopqrstuvwx",
        );
        for gone in [
            "sam.rivera",
            "example.com",
            "123456789012",
            "abcdefghijklmnopqrstuvwxyz0123456789",
            "abcdefghijklmnopqrstuvwx",
        ] {
            assert!(!scrubbed.contains(gone), "{gone} left in {scrubbed}");
        }
        assert!(scrubbed.contains("<email>"), "{scrubbed}");
        assert!(scrubbed.contains("<number>"), "{scrubbed}");
    }

    #[test]
    fn names_the_person_chose_go_whatever_their_case() {
        assert_eq!(
            words(
                "Could not reach Acme-Gateway: model acme-private-7b is not loaded on acme-gateway",
                &["acme-gateway", "acme-private-7b"],
                500
            ),
            "Could not reach <name>: model <name> is not loaded on <name>"
        );
        assert_eq!(words("model x failed", &["x"], 500), "model x failed");
    }

    #[test]
    fn one_line_of_at_most_the_length_asked() {
        assert_eq!(
            scrub("first line\n\n  second\tline"),
            "first line second line"
        );
        let long = "word ".repeat(200);
        let clipped = words(&long, &[], 50);
        assert_eq!(clipped.chars().count(), 50);
        assert!(clipped.ends_with('…'));
        assert_eq!(scrub("   "), "");
    }
}
