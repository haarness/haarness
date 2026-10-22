//! Secrets an MCP server's settings name as `${keychain:NAME}`, kept in the
//! operating system's store: the macOS Keychain, the Windows Credential
//! Manager or the Secret Service (GNOME Keyring, KWallet) on Linux. Haarness
//! reads one only when the server starts, never logs it and never writes it
//! back into a configuration file.
//!
//! Each store is reached through the program the system ships for it
//! (`security`, Windows PowerShell, `secret-tool`), with the secret passed
//! on standard input or output, never on a command line. On macOS that also
//! means an entry belongs to `security`, so reading it never asks again
//! after Haarness is updated, and one made with
//! `security add-generic-password -s haarness-mcp -a NAME -w` works too.

use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;

/// The service (macOS, Linux) or resource (Windows) every entry is kept
/// under; the entry's own name is its account.
pub const SERVICE: &str = "haarness-mcp";
/// The longest a lookup may take before the server fails, unless its
/// connection timeout is shorter.
pub const LOOKUP_LIMIT: Duration = Duration::from_secs(10);
/// A debug build reads and writes entries in this JSON file instead, so tests
/// never touch the person's own store.
const TEST_STORE: &str = "HAARNESS_TEST_KEYCHAIN_FILE";

/// Why an entry could not be read or written. Neither carries a secret.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The store has no entry by that name.
    Missing,
    /// The store could not be reached, or refused.
    Unavailable(String),
}

/// What people call the store on this system.
#[must_use]
pub fn store_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Keychain"
    } else if cfg!(windows) {
        "Credential Manager"
    } else {
        "keyring"
    }
}

/// Whether `name` can name an entry: letters, digits, `.`, `_` and `-`, at
/// most 128 of them, so it is never read as anything else on the way to the
/// store.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The entry named `name`.
pub async fn get(name: &str, limit: Duration) -> Result<String, Error> {
    check(name)?;
    if let Some(path) = test_store() {
        return read_test_store(&path)?.remove(name).ok_or(Error::Missing);
    }
    tokio::time::timeout(limit, platform::get(name))
        .await
        .map_err(|_| {
            Error::Unavailable(format!(
                "the {} did not answer within {} s",
                store_name(),
                limit.as_secs().max(1)
            ))
        })?
}

/// Stores `secret` as the entry named `name`, replacing one there.
pub async fn set(name: &str, secret: &str) -> Result<(), Error> {
    check(name)?;
    if secret.is_empty() {
        return Err(Error::Unavailable("an empty secret is not stored".into()));
    }
    if let Some(path) = test_store() {
        let mut entries = read_test_store(&path)?;
        entries.insert(name.to_owned(), secret.to_owned());
        return write_test_store(&path, &entries);
    }
    tokio::time::timeout(LOOKUP_LIMIT * 6, platform::set(name, secret))
        .await
        .map_err(|_| Error::Unavailable(format!("the {} did not answer", store_name())))?
}

/// Removes the entry named `name`; `false` when there was none.
pub async fn delete(name: &str) -> Result<bool, Error> {
    check(name)?;
    if let Some(path) = test_store() {
        let mut entries = read_test_store(&path)?;
        let removed = entries.remove(name).is_some();
        write_test_store(&path, &entries)?;
        return Ok(removed);
    }
    tokio::time::timeout(LOOKUP_LIMIT * 6, platform::delete(name))
        .await
        .map_err(|_| Error::Unavailable(format!("the {} did not answer", store_name())))?
}

fn check(name: &str) -> Result<(), Error> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(Error::Unavailable(format!(
            "\"{name}\" is not a usable name: use letters, digits, '.', '_' and '-'"
        )))
    }
}

fn test_store() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var_os(TEST_STORE)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn read_test_store(
    path: &std::path::Path,
) -> Result<std::collections::BTreeMap<String, String>, Error> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| Error::Unavailable("the test keychain file is not JSON".into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(std::collections::BTreeMap::new())
        }
        Err(error) => Err(Error::Unavailable(error.to_string())),
    }
}

fn write_test_store(
    path: &std::path::Path,
    entries: &std::collections::BTreeMap<String, String>,
) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(entries)
        .map_err(|error| Error::Unavailable(error.to_string()))?;
    std::fs::write(path, bytes).map_err(|error| Error::Unavailable(error.to_string()))
}

/// `bytes` as lower-case hex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Runs `program` with `input` on its standard input and returns its exit
/// code and standard output and error, killing it if the caller gives up.
async fn run(
    program: &str,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), Error> {
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            Error::Unavailable(if error.kind() == std::io::ErrorKind::NotFound {
                format!("{program} is not installed")
            } else {
                format!("{program} could not start")
            })
        })?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin
            .write_all(input)
            .await
            .map_err(|_| Error::Unavailable(format!("{program} stopped early")))?;
        drop(stdin);
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|_| Error::Unavailable(format!("{program} stopped early")))?;
    Ok((output.status.code(), output.stdout, output.stderr))
}

// Each store's code builds for tests on every system, so the code for the
// systems a build is not for is still checked.
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use password_vault as platform;
#[cfg(not(any(target_os = "macos", windows)))]
use secret_service as platform;

#[cfg(any(test, target_os = "macos"))]
#[cfg_attr(all(test, not(target_os = "macos")), allow(dead_code))]
mod macos {
    use super::{Error, SERVICE, run};

    const SECURITY: &str = "/usr/bin/security";
    /// `security`'s exit code for an item it could not find.
    const NOT_FOUND: i32 = 44;

    pub async fn get(name: &str) -> Result<String, Error> {
        // `-w` prints a password that happens to look like hex the same way
        // as an encoded one; `-g` labels the encoding (on standard error).
        // Neither output is ever shown on failure.
        let (code, _, stderr) = run(
            SECURITY,
            &["find-generic-password", "-s", SERVICE, "-a", name, "-g"],
            None,
        )
        .await?;
        match code {
            Some(0) => parse(&String::from_utf8_lossy(&stderr))
                .ok_or_else(|| Error::Unavailable("the Keychain entry could not be read".into())),
            Some(NOT_FOUND) => Err(Error::Missing),
            _ => Err(Error::Unavailable(
                "the Keychain refused to read the entry".into(),
            )),
        }
    }

    pub async fn set(name: &str, secret: &str) -> Result<(), Error> {
        // Commands on standard input keep the secret off the command line,
        // which other processes can read; hex keeps it free of quoting.
        let hex = super::hex(secret.as_bytes());
        let command = format!(
            "add-generic-password -U -s {SERVICE} -a {name} -l \"Haarness MCP: {name}\" -X {hex}\n"
        );
        let (code, _, stderr) = run(SECURITY, &["-i"], Some(command.as_bytes())).await?;
        if code != Some(0) || !stderr.is_empty() {
            return Err(Error::Unavailable(
                "the Keychain refused to store the entry".into(),
            ));
        }
        // Interactive mode reports success either way; reading it back
        // says whether it is there.
        match get(name).await {
            Ok(stored) if stored == secret => Ok(()),
            _ => Err(Error::Unavailable(
                "the Keychain did not keep the entry".into(),
            )),
        }
    }

    pub async fn delete(name: &str) -> Result<bool, Error> {
        let (code, _, _) = run(
            SECURITY,
            &["delete-generic-password", "-s", SERVICE, "-a", name],
            None,
        )
        .await?;
        match code {
            Some(0) => Ok(true),
            Some(NOT_FOUND) => Ok(false),
            _ => Err(Error::Unavailable(
                "the Keychain refused to remove the entry".into(),
            )),
        }
    }

    /// The password in `security -g`'s output: `password: "text"`,
    /// `password: 0x<hex>  "…"` for one with other bytes, or nothing for an
    /// empty one.
    pub(super) fn parse(stderr: &str) -> Option<String> {
        let line = stderr
            .lines()
            .find_map(|line| line.strip_prefix("password: "))
            .or_else(|| stderr.lines().find(|line| *line == "password:").map(|_| ""))?;
        if line.is_empty() {
            return Some(String::new());
        }
        if let Some(rest) = line.strip_prefix("0x") {
            let hex = rest.split_whitespace().next()?;
            if hex.len() % 2 != 0 {
                return None;
            }
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).ok())
                .collect::<Option<Vec<u8>>>()?;
            return String::from_utf8(bytes).ok();
        }
        line.strip_prefix('"')?.strip_suffix('"').map(str::to_owned)
    }
}

#[cfg(any(test, windows))]
#[cfg_attr(all(test, not(windows)), allow(dead_code))]
mod password_vault {
    use super::{Error, SERVICE, run};
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    /// Windows PowerShell reaches the Credential Manager's password vault
    /// without anything to install; the script arrives on standard input.
    async fn powershell(script: &str) -> Result<(Option<i32>, Vec<u8>), Error> {
        let script = format!(
            "$ErrorActionPreference = 'Stop'\n\
             [void][Windows.Security.Credentials.PasswordVault, Windows.Security.Credentials, ContentType = WindowsRuntime]\n\
             $vault = New-Object Windows.Security.Credentials.PasswordVault\n\
             {script}\n"
        );
        let (code, stdout, _) = run(
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "-",
            ],
            Some(script.as_bytes()),
        )
        .await?;
        Ok((code, stdout))
    }

    pub async fn get(name: &str) -> Result<String, Error> {
        let (code, stdout) = powershell(&format!(
            "try {{ $entry = $vault.Retrieve('{SERVICE}', '{name}') }} catch {{ exit 44 }}\n\
             $entry.RetrievePassword()\n\
             [Console]::Out.Write([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($entry.Password)))"
        ))
        .await?;
        match code {
            Some(0) => STANDARD
                .decode(String::from_utf8_lossy(&stdout).trim())
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .ok_or_else(|| {
                    Error::Unavailable("the Credential Manager entry could not be read".into())
                }),
            Some(44) => Err(Error::Missing),
            _ => Err(Error::Unavailable(
                "the Credential Manager refused to read the entry".into(),
            )),
        }
    }

    pub async fn set(name: &str, secret: &str) -> Result<(), Error> {
        let encoded = STANDARD.encode(secret.as_bytes());
        let (code, _) = powershell(&format!(
            "$secret = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}'))\n\
             try {{ $vault.Remove($vault.Retrieve('{SERVICE}', '{name}')) }} catch {{ }}\n\
             $vault.Add((New-Object Windows.Security.Credentials.PasswordCredential('{SERVICE}', '{name}', $secret)))"
        ))
        .await?;
        if code == Some(0) {
            Ok(())
        } else {
            Err(Error::Unavailable(
                "the Credential Manager refused to store the entry".into(),
            ))
        }
    }

    pub async fn delete(name: &str) -> Result<bool, Error> {
        let (code, _) = powershell(&format!(
            "try {{ $entry = $vault.Retrieve('{SERVICE}', '{name}') }} catch {{ exit 44 }}\n\
             $vault.Remove($entry)"
        ))
        .await?;
        match code {
            Some(0) => Ok(true),
            Some(44) => Ok(false),
            _ => Err(Error::Unavailable(
                "the Credential Manager refused to remove the entry".into(),
            )),
        }
    }
}

#[cfg(any(test, not(any(target_os = "macos", windows))))]
#[cfg_attr(all(test, any(target_os = "macos", windows)), allow(dead_code))]
mod secret_service {
    use super::{Error, SERVICE, run};

    /// libsecret's command-line tool (the `libsecret-tools` package), which
    /// speaks to the Secret Service of GNOME Keyring or KWallet.
    const SECRET_TOOL: &str = "secret-tool";

    fn unavailable(error: Error) -> Error {
        match error {
            Error::Unavailable(reason) if reason.ends_with("is not installed") => {
                Error::Unavailable(
                    "secret-tool is not installed (install libsecret-tools to use the keyring)"
                        .into(),
                )
            }
            other => other,
        }
    }

    pub async fn get(name: &str) -> Result<String, Error> {
        let (code, stdout, stderr) = run(
            SECRET_TOOL,
            &["lookup", "service", SERVICE, "account", name],
            None,
        )
        .await
        .map_err(unavailable)?;
        match code {
            Some(0) => String::from_utf8(stdout)
                .map_err(|_| Error::Unavailable("the keyring entry is not text".into())),
            // Not found is exit 1 with nothing said; a keyring that cannot
            // be reached says why on standard error.
            Some(1) if stderr.is_empty() => Err(Error::Missing),
            _ => Err(Error::Unavailable(
                "the keyring could not be reached (is a Secret Service running and unlocked?)"
                    .into(),
            )),
        }
    }

    pub async fn set(name: &str, secret: &str) -> Result<(), Error> {
        let label = format!("Haarness MCP: {name}");
        let (code, _, _) = run(
            SECRET_TOOL,
            &[
                "store", "--label", &label, "service", SERVICE, "account", name,
            ],
            Some(secret.as_bytes()),
        )
        .await
        .map_err(unavailable)?;
        if code == Some(0) {
            Ok(())
        } else {
            Err(Error::Unavailable(
                "the keyring refused to store the entry (is a Secret Service running and unlocked?)"
                    .into(),
            ))
        }
    }

    pub async fn delete(name: &str) -> Result<bool, Error> {
        let present = match get(name).await {
            Ok(_) => true,
            Err(Error::Missing) => false,
            Err(error) => return Err(error),
        };
        if !present {
            return Ok(false);
        }
        let (code, _, _) = run(
            SECRET_TOOL,
            &["clear", "service", SERVICE, "account", name],
            None,
        )
        .await
        .map_err(unavailable)?;
        if code == Some(0) {
            Ok(true)
        } else {
            Err(Error::Unavailable(
                "the keyring refused to remove the entry".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_plain_words_the_store_cannot_misread() {
        for name in ["github-token", "team.api_key", "A1"] {
            assert!(valid_name(name), "{name}");
        }
        for name in [
            "",
            "a b",
            "x'y",
            "x\"y",
            "a;b",
            "a\nb",
            "ü",
            &"x".repeat(129),
        ] {
            assert!(!valid_name(name), "{name:?}");
        }
    }

    #[test]
    fn security_output_reads_plain_encoded_and_empty_passwords() {
        use super::macos::parse;
        assert_eq!(parse("password: \"s3cret\"\n").as_deref(), Some("s3cret"));
        // `héllo "x"` comes back hex-encoded, with a rendering after it.
        let hex = super::hex("héllo \"x\"".as_bytes());
        assert_eq!(
            parse(&format!(
                "password: 0x{hex}  \"h\\303\\251llo \\\"x\\\"\"\n"
            ))
            .as_deref(),
            Some("héllo \"x\"")
        );
        assert_eq!(parse("password: \n").as_deref(), Some(""));
        assert_eq!(parse("password:\n").as_deref(), Some(""));
        assert_eq!(parse("keychain: \"x\"\n"), None);
        assert_eq!(parse("password: 0xabc  \"\"\n"), None);
    }
}
