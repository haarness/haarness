//! Whether a newer Haarness is out: the home screen says so on one line under
//! the tagline, with the command that installs it (`haarness --update`).
//!
//! It asks the release service `install.sh` installs from (and
//! `HAARNESS_RELEASE_URL` names, as for `haarness upgrade`) at most every
//! few hours, off the input loop: startup never waits for it, and a check
//! that fails says nothing. The answer is kept in `tui-state.toml`, so the
//! next launch shows it at once. A copy that came with Haarness Desktop is
//! Desktop's to update, and `HAARNESS_NO_UPDATE_CHECK=1` turns the check off.

use std::time::Duration;

/// How long an answer stands before the release service is asked again.
const CHECK_EVERY_SECS: i64 = 6 * 60 * 60;
const DEFAULT_RELEASE_URL: &str = "https://haarness.ai";

/// This build's version, the one `haarness --version` prints.
fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The release channel this build follows: preview for a prerelease
/// (`0.2.0-preview.1`), stable otherwise.
pub fn channel() -> &'static str {
    if current().contains('-') {
        "preview"
    } else {
        "stable"
    }
}

/// Whether this copy checks at all.
pub fn enabled() -> bool {
    if std::env::var("HAARNESS_NO_UPDATE_CHECK")
        .is_ok_and(|value| !matches!(value.trim(), "" | "0" | "false" | "no"))
    {
        return false;
    }
    // A development build checks only against a service a test names.
    if cfg!(debug_assertions) && release_url_setting().is_none() {
        return false;
    }
    !bundled_with_desktop()
}

/// A `haarness` inside Desktop (`…/Resources/bin/haarness` in the macOS app,
/// `resources/bin` beside the Linux and Windows app) updates with Desktop.
fn bundled_with_desktop() -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let executable = executable.canonicalize().unwrap_or(executable);
    let folder = executable.parent();
    let named = |path: Option<&std::path::Path>, name: &str| {
        path.and_then(std::path::Path::file_name)
            .is_some_and(|value| value.to_string_lossy().eq_ignore_ascii_case(name))
    };
    named(folder, "bin") && named(folder.and_then(std::path::Path::parent), "resources")
}

fn release_url_setting() -> Option<String> {
    std::env::var("HAARNESS_RELEASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
}

/// The release service: HTTPS, or HTTP on this computer (tests), as
/// `install.sh` accepts it.
fn release_url() -> Option<String> {
    let url = release_url_setting().unwrap_or_else(|| DEFAULT_RELEASE_URL.to_owned());
    let parsed = reqwest::Url::parse(&url).ok()?;
    let loopback = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    let allowed = parsed.username().is_empty()
        && parsed.password().is_none()
        && (parsed.scheme() == "https" || (parsed.scheme() == "http" && loopback));
    allowed.then_some(url)
}

/// The newer version the last check found, when it is still newer than this
/// build (it is not once this build is the update).
pub fn known() -> Option<String> {
    if !enabled() {
        return None;
    }
    crate::config::load_state()
        .latest_release
        .filter(|latest| newer(latest, current()))
}

/// Asks the release service for the newest version, unless a recent answer
/// stands; returns it when it is newer than this build.
pub async fn check() -> Option<String> {
    if !enabled() {
        return None;
    }
    let state = crate::config::load_state();
    let now = chrono::Utc::now().timestamp();
    if state
        .update_checked_at
        .is_some_and(|at| (0..CHECK_EVERY_SECS).contains(&(now - at)))
    {
        return state
            .latest_release
            .filter(|latest| newer(latest, current()));
    }
    let latest = latest_release().await?;
    crate::config::save_state(|state| {
        state.update_checked_at = Some(now);
        state.latest_release = Some(latest.clone());
    });
    newer(&latest, current()).then_some(latest)
}

/// The newest CLI release with a file for this computer, from the channel
/// `haarness --update` installs from: stable, or preview for a prerelease
/// build, so the line never offers what the command would not install.
async fn latest_release() -> Option<String> {
    let base = release_url()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let response = client
        .get(format!("{base}/api/v1/releases/{}/cli", channel()))
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    release_for_this_computer(&response.json::<serde_json::Value>().await.ok()?)
}

/// The CLI version of a release manifest, when it has a binary for this
/// system and architecture.
fn release_for_this_computer(body: &serde_json::Value) -> Option<String> {
    let os = std::env::consts::OS;
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => return None,
    };
    release_for(body, os, arch)
}

fn release_for(body: &serde_json::Value, os: &str, arch: &str) -> Option<String> {
    let manifest = body.get("manifest")?;
    let cli = manifest.get("products")?.get("cli")?;
    let version = cli
        .get("version")
        .or_else(|| manifest.get("version"))?
        .as_str()?;
    parse(version)?;
    let offered = cli.get("artifacts")?.as_array()?.iter().any(|artifact| {
        artifact.get("os").and_then(serde_json::Value::as_str) == Some(os)
            && artifact.get("arch").and_then(serde_json::Value::as_str) == Some(arch)
            && artifact.get("kind").and_then(serde_json::Value::as_str) == Some("binary")
    });
    offered.then(|| version.to_owned())
}

/// A version's numbers and prerelease identifiers.
fn parse(version: &str) -> Option<([u64; 3], Vec<&str>)> {
    let version = version.trim().trim_start_matches('v');
    let version = version.split('+').next()?;
    let (core, pre) = version
        .split_once('-')
        .map_or((version, None), |(core, pre)| (core, Some(pre)));
    let mut numbers = core.split('.');
    let mut next = || numbers.next()?.parse::<u64>().ok();
    let parsed = [next()?, next()?, next()?];
    if numbers.next().is_some() {
        return None;
    }
    let pre: Vec<&str> = pre.map(|pre| pre.split('.').collect()).unwrap_or_default();
    if pre.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some((parsed, pre))
}

/// SemVer: whether `a` is a later version than `b`.
fn newer(a: &str, b: &str) -> bool {
    let (Some((left, left_pre)), Some((right, right_pre))) = (parse(a), parse(b)) else {
        return false;
    };
    if left != right {
        return left > right;
    }
    // A release outranks its own prereleases.
    match (left_pre.is_empty(), right_pre.is_empty()) {
        (true, false) => return true,
        (_, true) => return false,
        (false, false) => {}
    }
    for (x, y) in left_pre.iter().zip(&right_pre) {
        let ordering = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if ordering.is_ne() {
            return ordering.is_gt();
        }
    }
    left_pre.len() > right_pre.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_semver() {
        assert!(newer("0.1.6", "0.1.5"));
        assert!(newer("0.2.0", "0.1.99"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(!newer("0.1.5", "0.1.5"));
        assert!(!newer("0.1.4", "0.1.5"));
        assert!(newer("0.2.0", "0.2.0-preview.3"));
        assert!(!newer("0.2.0-preview.3", "0.2.0"));
        assert!(newer("0.2.0-preview.10", "0.2.0-preview.9"));
        assert!(newer("0.2.0-preview.1", "0.2.0-preview"));
        assert!(newer("0.2.0-beta", "0.2.0-alpha"));
        assert!(!newer("v0.1.5", "0.1.5+build"));
        for bad in ["", "latest", "1.2", "1.2.3.4", "1.2.x", "1.2.3-"] {
            assert!(!newer(bad, "0.0.1"), "{bad}");
            assert!(!newer("9.9.9", bad), "{bad}");
        }
    }

    #[test]
    fn a_release_counts_only_with_a_binary_for_this_computer() {
        let body = serde_json::json!({
            "channel": "stable",
            "manifest": {
                "version": "0.1.6",
                "products": {
                    "cli": {
                        "version": "0.1.6",
                        "artifacts": [
                            { "os": "macos", "arch": "arm64", "kind": "binary" },
                            { "os": "linux", "arch": "x64", "kind": "binary" },
                        ]
                    }
                }
            }
        });
        assert_eq!(
            release_for(&body, "macos", "arm64").as_deref(),
            Some("0.1.6")
        );
        assert_eq!(release_for(&body, "linux", "x64").as_deref(), Some("0.1.6"));
        assert_eq!(release_for(&body, "windows", "x64"), None);
        assert_eq!(release_for(&body, "linux", "arm64"), None);
        assert_eq!(
            release_for(&serde_json::json!({ "error": "no" }), "macos", "arm64"),
            None
        );
        let odd = serde_json::json!({ "manifest": { "products": { "cli": {
            "version": "latest", "artifacts": [{ "os": "macos", "arch": "arm64", "kind": "binary" }]
        } } } });
        assert_eq!(release_for(&odd, "macos", "arm64"), None);
    }
}
