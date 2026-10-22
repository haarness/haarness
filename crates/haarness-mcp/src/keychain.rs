//! Secrets an MCP server's settings name as `${keychain:NAME}`, kept in the
//! operating system's store: the macOS Keychain, the Windows Credential
//! Manager or the Secret Service (GNOME Keyring, KWallet) on Linux. Haarness
//! reads one only when the server starts, never logs it and never writes it
//! back into a configuration file.
//!
//! macOS and Windows are reached through their own APIs (the `keyring`
//! crate), so a secret never passes through a command line, a script or
//! another program. A macOS entry Haarness stores belongs to Haarness: other
//! programs, `security` included, must ask the person before reading it, and
//! a release signed with the same Developer ID keeps reading it after an
//! update. Linux uses libsecret's `secret-tool`, with the secret on standard
//! input or output.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// The service (macOS, Linux) or the target's suffix (Windows) every entry is
/// kept under; the entry's own name is its account.
pub const SERVICE: &str = "haarness-mcp";
/// The longest a lookup may take before the server fails, unless its
/// connection timeout is shorter.
pub const LOOKUP_LIMIT: Duration = Duration::from_secs(10);
/// How long storing or removing an entry may take, a prompt the system shows
/// for it included.
const CHANGE_LIMIT: Duration = Duration::from_secs(60);
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

/// Whether `name` can name an entry: a letter or digit, then letters,
/// digits, `.`, `_` and `-`, at most 128 in all, so it is never read as
/// anything else on the way to the store.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    name.len() <= 128
        && name
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The entry named `name`.
pub async fn get(name: &str, limit: Duration) -> Result<String, Error> {
    get_in(test_store().as_deref(), name, limit).await
}

/// Stores `secret` as the entry named `name`, replacing one there.
pub async fn set(name: &str, secret: &str) -> Result<(), Error> {
    set_in(test_store().as_deref(), name, secret).await
}

/// Stores `secret` as a new entry, never replacing one: `name` when it is
/// free, otherwise the first free `name-2`, `name-3` … Returns the name used.
pub async fn add(name: &str, secret: &str) -> Result<String, Error> {
    add_in(test_store().as_deref(), name, secret).await
}

/// Removes the entry named `name`; `false` when there was none.
pub async fn delete(name: &str) -> Result<bool, Error> {
    delete_in(test_store().as_deref(), name).await
}

/// [`get`] from `store`, the test file, when there is one.
async fn get_in(store: Option<&Path>, name: &str, limit: Duration) -> Result<String, Error> {
    check(name)?;
    if let Some(path) = store {
        return read_test_store(path)?.remove(name).ok_or(Error::Missing);
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

async fn set_in(store: Option<&Path>, name: &str, secret: &str) -> Result<(), Error> {
    check(name)?;
    if secret.is_empty() {
        return Err(Error::Unavailable("an empty secret is not stored".into()));
    }
    if let Some(path) = store {
        let mut entries = read_test_store(path)?;
        entries.insert(name.to_owned(), secret.to_owned());
        return write_test_store(path, &entries);
    }
    tokio::time::timeout(CHANGE_LIMIT, platform::set(name, secret))
        .await
        .map_err(|_| Error::Unavailable(format!("the {} did not answer", store_name())))?
}

async fn add_in(store: Option<&Path>, name: &str, secret: &str) -> Result<String, Error> {
    check(name)?;
    for attempt in 1..=99 {
        let candidate = if attempt == 1 {
            name.to_owned()
        } else {
            let suffix = format!("-{attempt}");
            format!("{}{suffix}", &name[..name.len().min(128 - suffix.len())])
        };
        match get_in(store, &candidate, LOOKUP_LIMIT).await {
            Err(Error::Missing) => {
                set_in(store, &candidate, secret).await?;
                return Ok(candidate);
            }
            Ok(_) => {}
            Err(error) => return Err(error),
        }
    }
    Err(Error::Unavailable(format!(
        "every name like {name} is taken in the {}",
        store_name()
    )))
}

async fn delete_in(store: Option<&Path>, name: &str) -> Result<bool, Error> {
    check(name)?;
    if let Some(path) = store {
        let mut entries = read_test_store(path)?;
        let removed = entries.remove(name).is_some();
        write_test_store(path, &entries)?;
        return Ok(removed);
    }
    tokio::time::timeout(CHANGE_LIMIT, platform::delete(name))
        .await
        .map_err(|_| Error::Unavailable(format!("the {} did not answer", store_name())))?
}

fn check(name: &str) -> Result<(), Error> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(Error::Unavailable(format!(
            "\"{name}\" is not a usable name: start with a letter or digit, then use letters, digits, '.', '_' and '-'"
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

fn read_test_store(path: &Path) -> Result<std::collections::BTreeMap<String, String>, Error> {
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
    path: &Path,
    entries: &std::collections::BTreeMap<String, String>,
) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(entries)
        .map_err(|error| Error::Unavailable(error.to_string()))?;
    std::fs::write(path, bytes).map_err(|error| Error::Unavailable(error.to_string()))
}

#[cfg(any(target_os = "macos", windows))]
use native as platform;
#[cfg(not(any(target_os = "macos", windows)))]
use secret_service as platform;

/// The macOS Keychain (the login keychain) and the Windows Credential
/// Manager, through their APIs. Calls block, so each runs on a blocking
/// thread.
#[cfg(any(target_os = "macos", windows))]
mod native {
    use super::{Error, SERVICE, store_name};

    fn failure(error: keyring::Error) -> Error {
        match error {
            keyring::Error::NoEntry => Error::Missing,
            keyring::Error::TooLong(_, limit) => Error::Unavailable(format!(
                "the secret is longer than the {} keeps ({limit} bytes at most)",
                store_name()
            )),
            keyring::Error::BadEncoding(_) => {
                Error::Unavailable(format!("the {} entry is not text", store_name()))
            }
            keyring::Error::NoStorageAccess(_) => Error::Unavailable(format!(
                "the {} could not be opened (is it locked?)",
                store_name()
            )),
            other => Error::Unavailable(format!("the {} refused: {other}", store_name())),
        }
    }

    async fn blocking<T: Send + 'static>(
        work: impl FnOnce() -> Result<T, keyring::Error> + Send + 'static,
    ) -> Result<T, Error> {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|_| Error::Unavailable(format!("the {} stopped answering", store_name())))?
            .map_err(failure)
    }

    pub async fn get(name: &str) -> Result<String, Error> {
        let name = name.to_owned();
        blocking(move || keyring::Entry::new(SERVICE, &name)?.get_password()).await
    }

    pub async fn set(name: &str, secret: &str) -> Result<(), Error> {
        let (name, secret) = (name.to_owned(), secret.to_owned());
        blocking(move || keyring::Entry::new(SERVICE, &name)?.set_password(&secret)).await
    }

    pub async fn delete(name: &str) -> Result<bool, Error> {
        let name = name.to_owned();
        match blocking(move || keyring::Entry::new(SERVICE, &name)?.delete_credential()).await {
            Ok(()) => Ok(true),
            Err(Error::Missing) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// libsecret's command-line tool (the `libsecret-tools` package), which
/// speaks to the Secret Service of GNOME Keyring or KWallet. Its code builds
/// for tests everywhere, so it is checked on every system.
#[cfg(any(test, not(any(target_os = "macos", windows))))]
#[cfg_attr(all(test, any(target_os = "macos", windows)), allow(dead_code))]
mod secret_service {
    use super::{Error, SERVICE, run};

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

/// Runs `program` with `input` on its standard input and returns its exit
/// code and standard output and error, killing it if the caller gives up.
/// The input is written while the output is read, so neither side can wait
/// for the other.
#[cfg(any(test, not(any(target_os = "macos", windows))))]
async fn run(
    program: &str,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), Error> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
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
    let writer = input.zip(child.stdin.take()).map(|(input, mut stdin)| {
        let input = input.to_vec();
        tokio::spawn(async move {
            let written = stdin.write_all(&input).await;
            drop(stdin);
            written
        })
    });
    let output = child
        .wait_with_output()
        .await
        .map_err(|_| Error::Unavailable(format!("{program} stopped early")))?;
    if let Some(writer) = writer
        && !matches!(writer.await, Ok(Ok(())))
    {
        return Err(Error::Unavailable(format!("{program} stopped early")));
    }
    Ok((output.status.code(), output.stdout, output.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_start_with_a_letter_or_digit_and_are_plain_words_the_store_cannot_misread() {
        for name in ["github-token", "team.api_key", "A1", "1password"] {
            assert!(valid_name(name), "{name}");
        }
        for name in [
            "",
            "-token",
            ".hidden",
            "_x",
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

    /// What the store keeps comes back exactly: long secrets and ones with
    /// quotes and backslashes alike.
    #[tokio::test]
    async fn secrets_come_back_exactly_and_adding_never_replaces_one() {
        let path =
            std::env::temp_dir().join(format!("haarness-keychain-{}.json", uuid::Uuid::now_v7()));
        let store = Some(path.as_path());
        let long = "k".repeat(64 * 1024);
        let quoted = r#"a"b\c\"d'e $(f) `g` %h% ü"#;
        for secret in [long.as_str(), quoted] {
            set_in(store, "entry", secret).await.unwrap();
            assert_eq!(get_in(store, "entry", LOOKUP_LIMIT).await.unwrap(), secret);
        }
        // `add` leaves an entry there alone and takes the next free name.
        assert_eq!(add_in(store, "entry", "second").await.unwrap(), "entry-2");
        assert_eq!(add_in(store, "entry", "third").await.unwrap(), "entry-3");
        assert_eq!(get_in(store, "entry", LOOKUP_LIMIT).await.unwrap(), quoted);
        assert_eq!(
            get_in(store, "entry-2", LOOKUP_LIMIT).await.unwrap(),
            "second"
        );
        assert_eq!(add_in(store, "fresh", "x").await.unwrap(), "fresh");
        let long_name = "n".repeat(128);
        assert_eq!(add_in(store, &long_name, "x").await.unwrap(), long_name);
        let next = add_in(store, &long_name, "y").await.unwrap();
        assert!(next.len() <= 128 && next.ends_with("-2"), "{next}");
        assert!(delete_in(store, "entry-2").await.unwrap());
        assert!(!delete_in(store, "entry-2").await.unwrap());
        assert_eq!(
            get_in(store, "entry-2", LOOKUP_LIMIT).await,
            Err(Error::Missing)
        );
        std::fs::remove_file(path).unwrap();
    }

    /// A program that echoes a large input back: writing all of it before
    /// reading would fill both pipes and wait for ever.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_large_input_is_written_while_the_output_is_read() {
        let input = vec![b'x'; 4 * 1024 * 1024];
        let (code, stdout, _) =
            tokio::time::timeout(Duration::from_secs(20), run("cat", &[], Some(&input)))
                .await
                .expect("the input and output waited for each other")
                .unwrap();
        assert_eq!(code, Some(0));
        assert_eq!(stdout.len(), input.len());
    }
}
