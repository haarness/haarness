//! Chromium's native messaging registration: which browsers look where, the
//! host manifest, and the wrapper script it names. Everything here is a pure
//! function of its inputs so every platform's output is tested everywhere.
use super::{BrowserChoice, EXTENSION_ID, HOST_NAME};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Os {
    Mac,
    Linux,
    Windows,
}

impl Os {
    pub(crate) fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Linux
        }
    }
}

/// A Chromium browser and where it keeps its profile on each platform.
#[derive(Debug)]
pub(crate) struct Browser {
    pub(crate) choice: BrowserChoice,
    /// The `--browser` value and JSON `browser`.
    pub(crate) key: &'static str,
    pub(crate) name: &'static str,
    /// Under `~/Library/Application Support/`.
    mac: Option<&'static str>,
    /// Under the config home: `$CHROME_CONFIG_HOME`, `$XDG_CONFIG_HOME` or `~/.config/`.
    linux: Option<&'static str>,
    /// Under `HKCU\Software\` and `%LOCALAPPDATA%\` (its profile is `…\User Data`).
    windows: Option<(&'static str, &'static str)>,
    pub(crate) extensions_page: &'static str,
}

pub(crate) const BROWSERS: &[Browser] = &[
    Browser {
        choice: BrowserChoice::Chrome,
        key: "chrome",
        name: "Google Chrome",
        mac: Some("Google/Chrome"),
        linux: Some("google-chrome"),
        windows: Some(("Google\\Chrome", "Google\\Chrome")),
        extensions_page: "chrome://extensions",
    },
    Browser {
        choice: BrowserChoice::ChromeBeta,
        key: "chrome-beta",
        name: "Google Chrome Beta",
        mac: Some("Google/Chrome Beta"),
        linux: Some("google-chrome-beta"),
        // Every Chrome channel on Windows reads Google Chrome's key.
        windows: Some(("Google\\Chrome", "Google\\Chrome Beta")),
        extensions_page: "chrome://extensions",
    },
    Browser {
        choice: BrowserChoice::ChromeCanary,
        key: "chrome-canary",
        name: "Google Chrome Canary",
        mac: Some("Google/Chrome Canary"),
        linux: Some("google-chrome-unstable"),
        windows: Some(("Google\\Chrome", "Google\\Chrome SxS")),
        extensions_page: "chrome://extensions",
    },
    Browser {
        choice: BrowserChoice::ChromeForTesting,
        key: "chrome-for-testing",
        name: "Google Chrome for Testing",
        mac: Some("Google/Chrome for Testing"),
        linux: None,
        windows: None,
        extensions_page: "chrome://extensions",
    },
    Browser {
        choice: BrowserChoice::Chromium,
        key: "chromium",
        name: "Chromium",
        mac: Some("Chromium"),
        linux: Some("chromium"),
        windows: Some(("Chromium", "Chromium")),
        extensions_page: "chrome://extensions",
    },
    Browser {
        choice: BrowserChoice::Edge,
        key: "edge",
        name: "Microsoft Edge",
        mac: Some("Microsoft Edge"),
        linux: Some("microsoft-edge"),
        windows: Some(("Microsoft\\Edge", "Microsoft\\Edge")),
        extensions_page: "edge://extensions",
    },
    Browser {
        choice: BrowserChoice::Brave,
        key: "brave",
        name: "Brave",
        mac: Some("BraveSoftware/Brave-Browser"),
        linux: Some("BraveSoftware/Brave-Browser"),
        windows: Some((
            "BraveSoftware\\Brave-Browser",
            "BraveSoftware\\Brave-Browser",
        )),
        extensions_page: "brave://extensions",
    },
    Browser {
        choice: BrowserChoice::Vivaldi,
        key: "vivaldi",
        name: "Vivaldi",
        mac: Some("Vivaldi"),
        linux: Some("vivaldi"),
        windows: None,
        extensions_page: "vivaldi://extensions",
    },
];

pub(crate) fn browser(choice: BrowserChoice) -> &'static Browser {
    BROWSERS
        .iter()
        .find(|browser| browser.choice == choice)
        .expect("every choice has a browser")
}

/// Where a browser finds the host on one platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// `<profile root>/NativeMessagingHosts/ai.haarness.browser.json`.
    File { profile: PathBuf, manifest: PathBuf },
    /// A registry key whose default value names the manifest file.
    Registry { key: String, profile: PathBuf },
    /// Not on this platform.
    Unsupported,
}

pub(crate) fn manifest_file_name() -> String {
    format!("{HOST_NAME}.json")
}

/// Where `browser` reads host manifests. `home` is the home folder;
/// `local_app_data` is `%LOCALAPPDATA%` on Windows; `config_home` is the
/// folder Chromium keeps its profiles in on Linux when it is not
/// `~/.config` (see [`config_home`]).
pub(crate) fn place(
    browser: &Browser,
    os: Os,
    home: &Path,
    local_app_data: Option<&Path>,
    config_home: Option<&Path>,
) -> Place {
    let file = |profile: PathBuf| Place::File {
        manifest: profile
            .join("NativeMessagingHosts")
            .join(manifest_file_name()),
        profile,
    };
    match os {
        Os::Mac => browser.mac.map_or(Place::Unsupported, |folder| {
            file(home.join("Library/Application Support").join(folder))
        }),
        Os::Linux => browser.linux.map_or(Place::Unsupported, |folder| {
            file(
                config_home
                    .map_or_else(|| home.join(".config"), Path::to_path_buf)
                    .join(folder),
            )
        }),
        Os::Windows => match browser.windows {
            Some((vendor, profile)) => Place::Registry {
                key: registry_key(vendor),
                profile: local_app_data
                    .map_or_else(|| home.join("AppData\\Local"), Path::to_path_buf)
                    .join(profile)
                    .join("User Data"),
            },
            None => Place::Unsupported,
        },
    }
}

/// Where Chromium on Linux keeps its profiles (and so reads user-level host
/// manifests) instead of `~/.config`: `$CHROME_CONFIG_HOME`, else
/// `$XDG_CONFIG_HOME`. Like the XDG rules, only an absolute value counts.
pub(crate) fn config_home(
    chrome_config_home: Option<&str>,
    xdg_config_home: Option<&str>,
) -> Option<PathBuf> {
    [chrome_config_home, xdg_config_home]
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .find(|folder| folder.is_absolute())
}

pub(crate) fn registry_key(vendor: &str) -> String {
    format!("HKCU\\Software\\{vendor}\\NativeMessagingHosts\\{HOST_NAME}")
}

/// `reg` arguments that point `key` at `manifest`.
pub(crate) fn reg_add(key: &str, manifest: &Path) -> Vec<String> {
    vec![
        "add".into(),
        key.into(),
        "/ve".into(),
        "/t".into(),
        "REG_SZ".into(),
        "/d".into(),
        manifest.display().to_string(),
        "/f".into(),
    ]
}

pub(crate) fn reg_query(key: &str) -> Vec<String> {
    vec!["query".into(), key.into(), "/ve".into()]
}

pub(crate) fn reg_delete(key: &str) -> Vec<String> {
    vec!["delete".into(), key.into(), "/f".into()]
}

/// The default value in `reg query … /ve` output.
pub(crate) fn reg_default_value(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (_, value) = line.split_once("REG_SZ")?;
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// Browsers installed as Flatpaks or Snaps run sandboxed and cannot start a
/// program outside it. They are reported, never registered.
pub(crate) const SANDBOXED: &[(&str, &str, &str)] = &[
    (
        "chrome",
        "Google Chrome (Flatpak)",
        ".var/app/com.google.Chrome",
    ),
    (
        "chromium",
        "Chromium (Flatpak)",
        ".var/app/org.chromium.Chromium",
    ),
    (
        "edge",
        "Microsoft Edge (Flatpak)",
        ".var/app/com.microsoft.Edge",
    ),
    ("brave", "Brave (Flatpak)", ".var/app/com.brave.Browser"),
    (
        "vivaldi",
        "Vivaldi (Flatpak)",
        ".var/app/com.vivaldi.Vivaldi",
    ),
    ("chromium", "Chromium (Snap)", "snap/chromium"),
];

pub(crate) const SANDBOXED_NOTE: &str = "Browsers installed as a Flatpak or Snap cannot start Haarness yet. Install the browser from its website to use Haarness for Chrome.";

/// The host manifest Chrome reads. `extra` are more extension IDs allowed
/// to start the host (`HAARNESS_BROWSER_EXTENSION_IDS`).
pub(crate) fn manifest(wrapper: &Path, extra: &[String]) -> Value {
    let mut origins = vec![format!("chrome-extension://{EXTENSION_ID}/")];
    for id in extra {
        let origin = format!("chrome-extension://{id}/");
        if !origins.contains(&origin) {
            origins.push(origin);
        }
    }
    json!({
        "name": HOST_NAME,
        "description": "Haarness for Chrome",
        "path": wrapper.display().to_string(),
        "type": "stdio",
        "allowed_origins": origins,
    })
}

/// Extension IDs from a comma-separated list; anything that is not an ID
/// (32 letters a–p) is ignored.
pub(crate) fn extension_ids(list: Option<&str>) -> Vec<String> {
    let mut ids = Vec::new();
    for id in list.unwrap_or_default().split(',').map(str::trim) {
        if id.len() == 32
            && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
            && !ids.iter().any(|seen| seen == id)
        {
            ids.push(id.to_owned());
        }
    }
    ids
}

/// The variables a wrapper passes on, so the bridge resolves the data root
/// setup used even though Chrome starts it with Chrome's own environment.
pub(crate) const FORWARDED: &[&str] = &[
    "HAARNESS_HOME",
    "HAARNESS_DATA_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
];

pub(crate) fn wrapper_file_name(os: Os) -> &'static str {
    match os {
        Os::Windows => "haarness-browser-host.bat",
        Os::Mac | Os::Linux => "haarness-browser-host",
    }
}

/// `value` as one POSIX shell word.
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The wrapper Chrome runs on macOS and Linux.
pub(crate) fn shell_wrapper(exe: &Path, env: &[(&str, String)]) -> String {
    let mut script = String::from("#!/bin/sh\n");
    for (key, value) in env {
        let _ = writeln!(script, "export {key}={}", shell_quote(value));
    }
    let _ = writeln!(
        script,
        "exec {} browser native-host \"$@\"",
        shell_quote(&exe.display().to_string())
    );
    script
}

/// `value` for a batch file line: `%` doubled. Quotes and line breaks cannot
/// be escaped there, and no Windows path contains them.
pub(crate) fn batch_escape(value: &str) -> Result<String> {
    if value.contains(['"', '\r', '\n']) {
        bail!("{value} cannot be written into a batch file");
    }
    Ok(value.replace('%', "%%"))
}

/// `%~dp0`: in a batch file, the folder of the batch file itself, with a
/// trailing backslash, as cmd.exe was given it.
const BATCH_FOLDER: &str = "%~dp0";

/// `path` below `folder`, as a batch file names it after [`BATCH_FOLDER`]:
/// `bin\haarness.exe`, or `.` for the folder itself.
fn batch_relative(path: &Path, folder: &Path) -> Option<String> {
    let parts = path
        .strip_prefix(folder)
        .ok()?
        .iter()
        .map(|part| part.to_str())
        .collect::<Option<Vec<_>>>()?;
    Some(if parts.is_empty() {
        ".".into()
    } else {
        parts.join("\\")
    })
}

/// `..\..` from `folder` up to `ancestor`, when `ancestor` contains it.
fn batch_up(ancestor: &Path, folder: &Path) -> Option<String> {
    let depth = folder.strip_prefix(ancestor).ok()?.iter().count();
    Some(if depth == 0 {
        ".".into()
    } else {
        vec![".."; depth].join("\\")
    })
}

/// The wrapper Chrome runs on Windows, kept in `folder`.
///
/// cmd.exe reads a batch file in the console's code page (437, 850, 936…),
/// never UTF-8, while the paths here hold the user's profile folder, which may
/// be `C:\Users\José`. So the executable, which always sits in the wrapper's
/// own folder, is named relative to `%~dp0`, which cmd fills in from the path
/// Chrome started it with; a variable naming a folder the wrapper lives in is
/// built the same way. Any other value that is not ASCII is read after the
/// console switches to UTF-8.
pub(crate) fn batch_wrapper(exe: &Path, env: &[(&str, String)], folder: &Path) -> Result<String> {
    let mut lines = Vec::new();
    for (key, value) in env {
        lines.push(match batch_up(Path::new(value), folder) {
            Some(up) if !value.is_ascii() => {
                format!("for %%I in (\"{BATCH_FOLDER}{up}\") do set \"{key}=%%~fI\"")
            }
            _ => format!("set \"{key}={}\"", batch_escape(value)?),
        });
    }
    let target = match batch_relative(exe, folder) {
        Some(relative) => format!("{BATCH_FOLDER}{}", batch_escape(&relative)?),
        None => batch_escape(&exe.display().to_string())?,
    };
    lines.push(format!("\"{target}\" browser native-host %*"));
    let mut script = String::from("@echo off\r\n");
    if lines.iter().any(|line| !line.is_ascii()) {
        script.push_str("chcp 65001 >nul\r\n");
    }
    for line in lines {
        let _ = write!(script, "{line}\r\n");
    }
    Ok(script)
}

/// The wrapper script for `os`. `folder` is where it is written.
pub(crate) fn wrapper(os: Os, exe: &Path, env: &[(&str, String)], folder: &Path) -> Result<String> {
    match os {
        Os::Windows => batch_wrapper(exe, env, folder),
        Os::Mac | Os::Linux => Ok(shell_wrapper(exe, env)),
    }
}

/// The executable a wrapper this module wrote runs. `folder` is the
/// wrapper's own, which a batch file's `%~dp0` names.
pub(crate) fn wrapper_target(script: &str, folder: &Path) -> Option<PathBuf> {
    for line in script.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("exec ") {
            return shell_unquote(rest).map(PathBuf::from);
        }
        if let Some(rest) = line.strip_prefix('"')
            && line.ends_with("browser native-host %*")
        {
            let (exe, _) = rest.split_once('"')?;
            let exe = exe.replace("%%", "%");
            return Some(match exe.strip_prefix(BATCH_FOLDER) {
                Some(relative) => relative
                    .split('\\')
                    .filter(|part| !part.is_empty())
                    .fold(folder.to_path_buf(), |path, part| path.join(part)),
                None => PathBuf::from(exe),
            });
        }
    }
    None
}

/// The first single-quoted word of `text`, as `shell_quote` writes it.
fn shell_unquote(text: &str) -> Option<String> {
    let mut rest = text.strip_prefix('\'')?;
    let mut word = String::new();
    loop {
        let (part, after) = rest.split_once('\'')?;
        word.push_str(part);
        match after.strip_prefix("\\''") {
            Some(next) => {
                word.push('\'');
                rest = next;
            }
            None => return Some(word),
        }
    }
}

/// A build in a Cargo `target/debug` or `target/release` folder.
pub(crate) fn is_development_build(path: &Path) -> bool {
    let text = path.display().to_string().replace('\\', "/");
    text.contains("/target/debug/") || text.contains("/target/release/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_name_the_wrapper_and_allow_only_haarness_for_chrome() {
        let manifest = manifest(Path::new("/data/browser/chrome/haarness-browser-host"), &[]);
        assert_eq!(
            manifest,
            json!({"name":"ai.haarness.browser","description":"Haarness for Chrome",
                "path":"/data/browser/chrome/haarness-browser-host","type":"stdio",
                "allowed_origins":["chrome-extension://accdjhmcgljbadpeanajllmcembhkomn/"]})
        );
        let ids = extension_ids(Some(
            " gioamnpfejaiiikckfpnmjkfflndgnkh,not-an-id, accdjhmcgljbadpeanajllmcembhkomn,gioamnpfejaiiikckfpnmjkfflndgnkh,",
        ));
        assert_eq!(
            ids,
            [
                "gioamnpfejaiiikckfpnmjkfflndgnkh",
                "accdjhmcgljbadpeanajllmcembhkomn"
            ]
        );
        let manifest = super::manifest(Path::new("/w"), &ids);
        assert_eq!(
            manifest["allowed_origins"],
            json!([
                "chrome-extension://accdjhmcgljbadpeanajllmcembhkomn/",
                "chrome-extension://gioamnpfejaiiikckfpnmjkfflndgnkh/"
            ])
        );
        assert!(extension_ids(None).is_empty());
    }

    #[test]
    fn shell_wrappers_quote_every_value_and_name_their_target() {
        let exe = Path::new("/Users/o'brien/My Apps/haarness");
        let script = shell_wrapper(
            exe,
            &[
                ("HAARNESS_DATA_DIR", "/tmp/it's here".into()),
                ("XDG_CACHE_HOME", "/x/$HOME/`id`".into()),
            ],
        );
        assert_eq!(
            script,
            "#!/bin/sh\nexport HAARNESS_DATA_DIR='/tmp/it'\\''s here'\nexport XDG_CACHE_HOME='/x/$HOME/`id`'\nexec '/Users/o'\\''brien/My Apps/haarness' browser native-host \"$@\"\n"
        );
        let folder = Path::new("/data/browser/chrome");
        assert_eq!(wrapper_target(&script, folder).as_deref(), Some(exe));
        assert_eq!(
            wrapper_target(
                "#!/bin/sh\nexec '/usr/bin/haarness' browser native-host \"$@\"\n",
                folder
            )
            .as_deref(),
            Some(Path::new("/usr/bin/haarness"))
        );
        assert_eq!(wrapper_target("#!/bin/sh\necho hi\n", folder), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_wrapper_passes_its_values_through_sh_unchanged() {
        let value = "/tmp/a b/'quoted' $HOME `x` \\ \"double\"";
        let script = shell_wrapper(
            Path::new("/bin/echo"),
            &[("HAARNESS_DATA_DIR", value.into())],
        );
        let script = script.replace(
            "exec '/bin/echo' browser native-host \"$@\"",
            "printf %s \"$HAARNESS_DATA_DIR\"",
        );
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap(), value);
    }

    /// `root` joined one part at a time, as setup joins Windows paths, so the
    /// same test runs on every platform.
    fn windows_path(root: &str, parts: &[&str]) -> PathBuf {
        parts
            .iter()
            .fold(PathBuf::from(root), |path, part| path.join(part))
    }

    #[test]
    fn batch_wrappers_double_percent_signs_and_refuse_quotes() {
        let folder = windows_path(
            "C:\\Users\\a%b",
            &[".local", "share", "haarness", "browser", "chrome"],
        );
        let exe = folder.join("bin").join("haarness-0.1.1.exe");
        let script = batch_wrapper(
            &exe,
            &[("HAARNESS_DATA_DIR", "C:\\Data & 100%".into())],
            &folder,
        )
        .unwrap();
        assert_eq!(
            script,
            "@echo off\r\nset \"HAARNESS_DATA_DIR=C:\\Data & 100%%\"\r\n\"%~dp0bin\\haarness-0.1.1.exe\" browser native-host %*\r\n"
        );
        assert_eq!(wrapper_target(&script, &folder), Some(exe));
        // An executable elsewhere is named in full, as wrappers before `%~dp0` did.
        let elsewhere =
            batch_wrapper(Path::new("C:\\Tools\\a%b\\haarness.exe"), &[], &folder).unwrap();
        assert_eq!(
            elsewhere,
            "@echo off\r\n\"C:\\Tools\\a%%b\\haarness.exe\" browser native-host %*\r\n"
        );
        assert_eq!(
            wrapper_target(&elsewhere, &folder).as_deref(),
            Some(Path::new("C:\\Tools\\a%b\\haarness.exe"))
        );
        assert!(batch_wrapper(Path::new("C:\\x\"y.exe"), &[], &folder).is_err());
        assert!(batch_escape("a\nb").is_err());
    }

    #[test]
    fn batch_wrappers_keep_non_ascii_paths_out_of_the_code_page() {
        // cmd.exe would read `José` in the OEM code page and miss the file.
        let root = windows_path("C:\\Users\\José", &["Haarness Test"]);
        let folder = windows_path(
            "C:\\Users\\José",
            &["Haarness Test", "data", "browser", "chrome"],
        );
        let exe = folder.join("bin").join("haarness-0.1.1.exe");
        let script = batch_wrapper(
            &exe,
            &[("HAARNESS_DATA_DIR", root.display().to_string())],
            &folder,
        )
        .unwrap();
        assert!(script.is_ascii(), "{script}");
        assert_eq!(
            script,
            "@echo off\r\nfor %%I in (\"%~dp0..\\..\\..\") do set \"HAARNESS_DATA_DIR=%%~fI\"\r\n\"%~dp0bin\\haarness-0.1.1.exe\" browser native-host %*\r\n"
        );
        assert_eq!(wrapper_target(&script, &folder), Some(exe.clone()));
        // A value outside the wrapper's folders is read as UTF-8.
        let script = batch_wrapper(
            &exe,
            &[("XDG_CACHE_HOME", "C:\\Users\\José\\cache".into())],
            &folder,
        )
        .unwrap();
        assert_eq!(
            script,
            "@echo off\r\nchcp 65001 >nul\r\nset \"XDG_CACHE_HOME=C:\\Users\\José\\cache\"\r\n\"%~dp0bin\\haarness-0.1.1.exe\" browser native-host %*\r\n"
        );
        assert_eq!(wrapper_target(&script, &folder), Some(exe));
    }

    #[test]
    fn registry_commands_point_each_vendor_key_at_one_manifest() {
        let manifest = Path::new(
            "C:\\Users\\a\\.local\\share\\haarness\\browser\\chrome\\ai.haarness.browser.json",
        );
        let key = registry_key("Google\\Chrome");
        assert_eq!(
            key,
            "HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\ai.haarness.browser"
        );
        assert_eq!(
            reg_add(&key, manifest),
            [
                "add",
                &key,
                "/ve",
                "/t",
                "REG_SZ",
                "/d",
                &manifest.display().to_string(),
                "/f"
            ]
        );
        assert_eq!(reg_query(&key), ["query", key.as_str(), "/ve"]);
        assert_eq!(reg_delete(&key), ["delete", key.as_str(), "/f"]);
        let output = "\r\nHKEY_CURRENT_USER\\Software\\Google\\Chrome\\NativeMessagingHosts\\ai.haarness.browser\r\n    (Default)    REG_SZ    C:\\Users\\a\\My Data\\ai.haarness.browser.json\r\n\r\n";
        assert_eq!(
            reg_default_value(output).as_deref(),
            Some("C:\\Users\\a\\My Data\\ai.haarness.browser.json")
        );
        assert_eq!(
            reg_default_value("ERROR: The system was unable to find the specified registry key"),
            None
        );
    }

    #[test]
    fn each_platform_has_its_own_manifest_places() {
        let home = Path::new("/home/a");
        let chrome = browser(BrowserChoice::Chrome);
        assert_eq!(
            place(chrome, Os::Mac, Path::new("/Users/a"), None, None),
            Place::File {
                profile: PathBuf::from("/Users/a/Library/Application Support/Google/Chrome"),
                manifest: PathBuf::from(
                    "/Users/a/Library/Application Support/Google/Chrome/NativeMessagingHosts/ai.haarness.browser.json"
                ),
            }
        );
        assert_eq!(
            place(
                browser(BrowserChoice::ChromeCanary),
                Os::Linux,
                home,
                None,
                None
            ),
            Place::File {
                profile: PathBuf::from("/home/a/.config/google-chrome-unstable"),
                manifest: PathBuf::from(
                    "/home/a/.config/google-chrome-unstable/NativeMessagingHosts/ai.haarness.browser.json"
                ),
            }
        );
        assert_eq!(
            place(browser(BrowserChoice::Brave), Os::Linux, home, None, None),
            Place::File {
                profile: PathBuf::from("/home/a/.config/BraveSoftware/Brave-Browser"),
                manifest: PathBuf::from(
                    "/home/a/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts/ai.haarness.browser.json"
                ),
            }
        );
        assert_eq!(
            place(
                browser(BrowserChoice::Edge),
                Os::Windows,
                home,
                Some(Path::new("C:\\L")),
                None
            ),
            Place::Registry {
                key: "HKCU\\Software\\Microsoft\\Edge\\NativeMessagingHosts\\ai.haarness.browser"
                    .into(),
                profile: Path::new("C:\\L").join("Microsoft\\Edge").join("User Data"),
            }
        );
        assert_eq!(
            place(
                browser(BrowserChoice::ChromeForTesting),
                Os::Linux,
                home,
                None,
                None
            ),
            Place::Unsupported
        );
        assert_eq!(
            place(
                browser(BrowserChoice::Vivaldi),
                Os::Windows,
                home,
                None,
                None
            ),
            Place::Unsupported
        );
        let pages: Vec<_> = BROWSERS
            .iter()
            .map(|browser| browser.extensions_page)
            .collect();
        assert_eq!(
            pages,
            [
                "chrome://extensions",
                "chrome://extensions",
                "chrome://extensions",
                "chrome://extensions",
                "chrome://extensions",
                "edge://extensions",
                "brave://extensions",
                "vivaldi://extensions"
            ]
        );
    }

    #[test]
    fn linux_follows_chromes_config_home() {
        // Absolute on every platform, so the test runs on Windows too.
        let base = std::env::temp_dir();
        let chrome = base.join("chrome").display().to_string();
        let xdg = base.join("xdg").display().to_string();
        assert_eq!(
            config_home(Some(&chrome), Some(&xdg)),
            Some(PathBuf::from(&chrome))
        );
        assert_eq!(config_home(None, Some(&xdg)), Some(PathBuf::from(&xdg)));
        // Relative values mean nothing to a browser started elsewhere.
        assert_eq!(
            config_home(Some("rel"), Some(&xdg)),
            Some(PathBuf::from(&xdg))
        );
        assert_eq!(config_home(None, Some("rel")), None);
        let xdg = Path::new("/home/a/.xdg/config");
        assert_eq!(
            place(
                browser(BrowserChoice::Chrome),
                Os::Linux,
                Path::new("/home/a"),
                None,
                Some(xdg)
            ),
            Place::File {
                profile: xdg.join("google-chrome"),
                manifest: xdg.join("google-chrome/NativeMessagingHosts/ai.haarness.browser.json"),
            }
        );
        // Only Linux reads it.
        assert_eq!(
            place(
                browser(BrowserChoice::Chrome),
                Os::Mac,
                Path::new("/Users/a"),
                None,
                Some(xdg)
            ),
            place(
                browser(BrowserChoice::Chrome),
                Os::Mac,
                Path::new("/Users/a"),
                None,
                None
            )
        );
    }

    #[test]
    fn development_builds_are_recognised_on_every_platform() {
        assert!(is_development_build(Path::new(
            "/w/haarness/target/debug/haarness"
        )));
        assert!(is_development_build(Path::new(
            "C:\\w\\target\\release\\haarness.exe"
        )));
        assert!(!is_development_build(Path::new(
            "/Applications/Haarness.app/Contents/Resources/bin/haarness"
        )));
        assert!(!is_development_build(Path::new(
            "/home/a/.local/bin/haarness"
        )));
    }
}
