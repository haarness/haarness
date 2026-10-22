//! Links that came from outside, such as a server's sign-in or
//! authorization link, and that Haarness opens in a browser or prints.

/// The address to open or print for a link a server supplied, or `None`
/// unless it is an http or https URL. Any other scheme reaches a local
/// handler: `file:` opens or launches a local file and a custom scheme
/// starts the app that registered it. The URL parser reads the text first
/// (scheme case, surrounding spaces, control characters), so the caller gets
/// the parsed address back, never the server's raw text: a control character
/// such as ESC is percent-encoded before it can reach a terminal.
#[must_use]
pub fn http_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then(|| url.to_string())
}

/// The scheme of a link [`http_url`] refused, to name it in an error (`file:`),
/// or `None` when the text is not a URL at all.
#[must_use]
pub fn scheme(value: &str) -> Option<String> {
    url::Url::parse(value)
        .ok()
        .map(|url| format!("{}:", url.scheme()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_web_link_comes_back_and_it_comes_back_parsed() {
        assert_eq!(
            http_url("https://auth.fixture.test/authorize?state=s").as_deref(),
            Some("https://auth.fixture.test/authorize?state=s")
        );
        assert_eq!(
            http_url(" HTTP://Auth.Fixture.Test/x ").as_deref(),
            Some("http://auth.fixture.test/x")
        );
        assert_eq!(
            http_url("https://auth.fixture.test/\u{1b}]52;c;eA==\u{7}x").as_deref(),
            Some("https://auth.fixture.test/%1B]52;c;eA==%07x")
        );
        for refused in [
            "file:///Applications/Calculator.app",
            "x-fixture-app://open",
            "javascript:alert(1)",
            "mailto:someone@fixture.test",
            "smb://share.fixture.test/x",
            "http:",
            "not a link",
            "",
        ] {
            assert_eq!(http_url(refused), None, "{refused}");
        }
        assert_eq!(
            scheme("file:///Applications/Calculator.app").as_deref(),
            Some("file:")
        );
        assert_eq!(scheme("not a link"), None);
    }
}
