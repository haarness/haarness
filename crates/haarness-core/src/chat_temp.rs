//! Each chat's own temp folder: where the commands of a conversation (the
//! chat at its root and every agent below it) keep their temporary files.
//! They get it as `TMPDIR`, `TMP` and `TEMP`, and the shell and file tools
//! reach it without asking about a folder outside the project (see
//! `permissions::bash::with_chat_temp`). Another chat's folder is outside
//! like any other.
//!
//! A folder is made the first time a command of the chat runs, for this user
//! alone. A server, as it starts, takes away the folders of the chats it
//! holds archived, and of chats it does not hold (another server's, or one
//! whose data folder is gone), once nothing changed in them for
//! [`IDLE_LIMIT`]: archiving can be undone, and the files are there again.
//!
//! The path is short on purpose. Python's `multiprocessing`, `ssh-agent` and
//! other programs make Unix sockets in `TMPDIR`, and macOS refuses a socket
//! path longer than 104 bytes, which a folder in macOS's own temp folder
//! (`/var/folders/…/T/`) or one named by a whole chat id would leave too
//! little room for. So the folders are in `/tmp/haarness-<uid>/chats`
//! (checked as [`crate::paths::own_private_folder`] checks a folder), each
//! named by the last eight hex digits of its chat's id, or a longer end of
//! it when another chat holds that name. On Windows they are in the user's
//! own temp folder. A test run names a folder of its own instead
//! ([`TEST_FOLDER_ENV`]).
//!
//! Beside each folder, `<name>.chat` records the chat it is for. Only a
//! folder so recorded is ever taken away, only for the chat its record
//! names, only when it is this user's, and never through a link.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use haarness_protocol::ThreadId;

/// The variables a chat's shell commands find its folder in.
pub const VARIABLES: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];

/// How long the folder of an archived chat, or of a chat the server does
/// not hold, is kept after anything in it, or its record, last changed.
pub const IDLE_LIMIT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Names the folder a test run's chats' folders go in (`<it>/chats`), in
/// place of `/tmp/haarness-<uid>`, so a run never leaves folders in the one
/// a person's servers share. Read only by a debug build or under the test
/// providers (`HAARNESS_TEST_PROVIDER`), never by a person's release build.
pub const TEST_FOLDER_ENV: &str = "HAARNESS_TEST_CHAT_TEMP";

/// How a record's file name ends.
const RECORD: &str = ".chat";

/// How many hex digits of the chat's id each name a folder may have holds,
/// tried in this order. The last ones of an id are random (the first ones
/// are its time), and even the longest leaves room below the socket limit.
const NAME_LENGTHS: [usize; 4] = [8, 12, 16, 20];

/// How many files and folders [`Recorded::untouched`] and the cleanup look
/// at in one folder; past it a folder counts as in use.
const MAX_LOOKED_AT: usize = 20_000;

/// One claim at a time in this process, so two calls of one chat never take
/// two names.
static CLAIMS: Mutex<()> = Mutex::new(());

/// The folder of the conversation whose root chat is `root`, made when it is
/// missing (or when the chat's own commands took it away), as the system
/// spells it. `None` when it cannot be made as this user's alone: the
/// chat's commands then keep the server's temp folder, as before.
#[must_use]
pub fn folder(root: ThreadId) -> Option<PathBuf> {
    folder_in(&chats_folder(true)?, root)
}

/// [`folder`] when it is there already, without making anything.
#[must_use]
pub fn existing(root: ThreadId) -> Option<PathBuf> {
    existing_in(&chats_folder(false)?, root)
}

/// Takes away the folder recorded for `root`, with everything in it, and its
/// record. Whether one went.
pub fn remove(root: ThreadId) -> bool {
    chats_folder(false).is_some_and(|chats| remove_in(&chats, root))
}

/// Every recorded folder, for the cleanup at start.
#[must_use]
pub fn recorded() -> Vec<Recorded> {
    chats_folder(false)
        .map(|chats| recorded_in(&chats))
        .unwrap_or_default()
}

/// A recorded folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    chat: ThreadId,
    chats: PathBuf,
    name: String,
}

impl Recorded {
    /// The chat at the root of the conversation the folder is for.
    #[must_use]
    pub fn chat(&self) -> ThreadId {
        self.chat
    }

    /// How long since anything in the folder changed, or a command of its
    /// chat last started (which touches its record). A folder too large to
    /// look through counts as just used.
    #[must_use]
    pub fn untouched(&self, now: SystemTime) -> Duration {
        let mut newest = modified(&record(&self.chats, &self.name));
        let mut pending = vec![self.chats.join(&self.name)];
        let mut looked = 0;
        while let Some(path) = pending.pop() {
            looked += 1;
            if looked > MAX_LOOKED_AT {
                return Duration::ZERO;
            }
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if let Ok(time) = metadata.modified() {
                newest = newest.max(time);
            }
            if metadata.is_dir()
                && let Ok(entries) = std::fs::read_dir(&path)
            {
                pending.extend(entries.flatten().map(|entry| entry.path()));
            }
        }
        now.duration_since(newest).unwrap_or_default()
    }
}

/// The names the folder of `root` may have, in the order they are tried.
fn names(root: ThreadId) -> [String; 4] {
    let hex = root.0.simple().to_string();
    NAME_LENGTHS.map(|length| hex[hex.len() - length..].to_owned())
}

fn record(chats: &Path, name: &str) -> PathBuf {
    chats.join(format!("{name}{RECORD}"))
}

/// The chat the record `name` in `chats` names: a file, not a link, that
/// holds its id.
fn recorded_chat(chats: &Path, name: &str) -> Option<ThreadId> {
    let path = record(chats, name);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > 64 {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    uuid::Uuid::parse_str(text.trim()).ok().map(ThreadId)
}

fn folder_in(chats: &Path, root: ThreadId) -> Option<PathBuf> {
    let _claim = CLAIMS.lock().unwrap_or_else(PoisonError::into_inner);
    let names = names(root);
    if let Some(name) = names
        .iter()
        .find(|name| recorded_chat(chats, name) == Some(root))
    {
        return ready(chats, name);
    }
    for name in &names {
        match claim(chats, name, root) {
            Ok(true) => match make_folder(&chats.join(name)) {
                Ok(()) => return ready(chats, name),
                // The name was free, so a folder there is no chat's, and not
                // this one's to take.
                Err(error) => {
                    let _ = std::fs::remove_file(record(chats, name));
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return None;
                    }
                }
            },
            // Another process recorded it for this chat just now.
            Ok(false) if recorded_chat(chats, name) == Some(root) => return ready(chats, name),
            Ok(false) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Writes the record `name` for `root` unless one is there: whether this
/// call made it.
fn claim(chats: &Path, name: &str, root: ThreadId) -> io::Result<bool> {
    use io::Write as _;
    let path = record(chats, name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    if let Err(error) = file.write_all(root.to_string().as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(true)
}

/// The chat's folder `name` in `chats`, made again when the chat's own
/// commands took it away or put something else in its place (the entry
/// itself goes then, never what a link there leads to).
fn ready(chats: &Path, name: &str) -> Option<PathBuf> {
    let folder = chats.join(name);
    match std::fs::symlink_metadata(&folder) {
        Ok(metadata) if metadata.is_dir() => {
            if !owned(&metadata) {
                return None;
            }
        }
        Ok(_) => {
            remove_entry(&folder).ok()?;
            make_folder(&folder).ok()?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => make_folder(&folder).ok()?,
        Err(_) => return None,
    }
    // A command of the chat starts: the cleanup keeps the folder while its
    // chat uses it, whether or not the command writes there.
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .open(record(chats, name))
        .and_then(|file| file.set_modified(SystemTime::now()));
    Some(folder)
}

fn existing_in(chats: &Path, root: ThreadId) -> Option<PathBuf> {
    let name = names(root)
        .into_iter()
        .find(|name| recorded_chat(chats, name) == Some(root))?;
    let folder = chats.join(name);
    std::fs::symlink_metadata(&folder)
        .ok()
        .filter(|metadata| metadata.is_dir() && owned(metadata))
        .map(|_| folder)
}

fn remove_in(chats: &Path, root: ThreadId) -> bool {
    let mut removed = false;
    for name in names(root) {
        if recorded_chat(chats, &name) != Some(root) {
            continue;
        }
        if take_away(&chats.join(&name)).is_ok()
            && std::fs::remove_file(record(chats, &name)).is_ok()
        {
            removed = true;
        }
    }
    removed
}

fn recorded_in(chats: &Path) -> Vec<Recorded> {
    let Ok(entries) = std::fs::read_dir(chats) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name();
            let name = file.to_str()?.strip_suffix(RECORD)?;
            let chat = recorded_chat(chats, name)?;
            names(chat).iter().any(|own| own == name).then(|| Recorded {
                chat,
                chats: chats.to_path_buf(),
                name: name.to_owned(),
            })
        })
        .collect()
}

/// Takes away the folder at `path` with everything in it, when it is this
/// user's: a link or a file there is the entry itself, and the links in it
/// go as links (`remove_dir_all` never follows one).
fn take_away(path: &Path) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() {
        return remove_entry(path);
    }
    if !owned(&metadata) {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    std::fs::remove_dir_all(path).or_else(|_| {
        open_up(path);
        std::fs::remove_dir_all(path)
    })
}

/// Gives this user back the right to empty the folders in `folder` that a
/// command closed (`chmod -w`), only those the listing says are folders,
/// never a link.
#[cfg(unix)]
fn open_up(folder: &Path) {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let user = nix::unistd::geteuid().as_raw();
    let mut pending = vec![folder.to_path_buf()];
    let mut looked = 0;
    while let Some(folder) = pending.pop() {
        if let Ok(metadata) = std::fs::symlink_metadata(&folder)
            && metadata.is_dir()
            && metadata.uid() == user
        {
            let mode = metadata.permissions().mode() | 0o700;
            let _ = std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(mode));
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            looked += 1;
            if looked > MAX_LOOKED_AT {
                return;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(entry.path());
            }
        }
    }
}

/// Windows' own removal clears a file's read-only mark.
#[cfg(not(unix))]
fn open_up(_: &Path) {}

/// A link or a file: the entry itself (on Windows a link to a folder is
/// taken away as a folder).
fn remove_entry(path: &Path) -> io::Result<()> {
    std::fs::remove_file(path).or_else(|error| {
        if cfg!(windows) {
            std::fs::remove_dir(path)
        } else {
            Err(error)
        }
    })
}

#[cfg(unix)]
fn make_folder(folder: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new().mode(0o700).create(folder)
}

#[cfg(not(unix))]
fn make_folder(folder: &Path) -> io::Result<()> {
    std::fs::create_dir(folder)
}

/// Whether this user owns what `metadata` describes (on Windows the temp
/// folder is the user's own).
#[cfg(unix)]
fn owned(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    metadata.uid() == nix::unistd::geteuid().as_raw()
}

#[cfg(not(unix))]
fn owned(_: &std::fs::Metadata) -> bool {
    true
}

fn modified(path: &Path) -> SystemTime {
    std::fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The folder all chats' folders are in, made when `make` and missing;
/// `None` when it is not this user's alone. A test run's is in the folder
/// [`TEST_FOLDER_ENV`] names, and a test binary has one of its own.
fn chats_folder(make: bool) -> Option<PathBuf> {
    let base = if let Some(named) = named_test_folder() {
        // Named: that folder or none, never the shared one.
        if !named.is_absolute() {
            return None;
        }
        let folder = crate::paths::canonicalize(named).ok()?;
        if !private(&folder, false) {
            return None;
        }
        folder
    } else if haarness_protocol::test_binary() {
        let folder = haarness_protocol::test_folder();
        if make {
            std::fs::create_dir_all(&folder).ok()?;
        }
        crate::paths::canonicalize(folder).ok()?
    } else {
        shared_base(make)?
    };
    let chats = base.join("chats");
    private(&chats, make).then_some(chats)
}

/// The folder [`TEST_FOLDER_ENV`] names, when this build reads it.
fn named_test_folder() -> Option<PathBuf> {
    if !cfg!(debug_assertions) && std::env::var_os("HAARNESS_TEST_PROVIDER").is_none() {
        return None;
    }
    std::env::var_os(TEST_FOLDER_ENV).map(PathBuf::from)
}

/// Whether `folder` is this user's alone, made so when `make` and missing.
#[cfg(unix)]
fn private(folder: &Path, make: bool) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    if make {
        return crate::paths::own_private_folder(folder);
    }
    std::fs::symlink_metadata(folder).is_ok_and(|metadata| {
        let open_to_others = metadata.mode() & 0o077 != 0;
        metadata.is_dir() && owned(&metadata) && !open_to_others
    })
}

#[cfg(not(unix))]
fn private(folder: &Path, make: bool) -> bool {
    if make && std::fs::create_dir_all(folder).is_err() {
        return false;
    }
    std::fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir())
}

/// `/tmp/haarness-<uid>` as the system spells it (`/private/tmp/…` on a
/// Mac); when someone else holds that name, a `haarness-<uid>-<8 hex>` this
/// process made for itself.
#[cfg(unix)]
fn shared_base(make: bool) -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt as _;
    static OWN: Mutex<Option<PathBuf>> = Mutex::new(None);
    let temp = crate::paths::canonicalize("/tmp").ok()?;
    let user = nix::unistd::geteuid().as_raw();
    let named = temp.join(format!("haarness-{user}"));
    if private(&named, make) {
        return Some(named);
    }
    let mut own = OWN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(folder) = own.as_ref().filter(|folder| private(folder, make)) {
        return Some(folder.clone());
    }
    if !make {
        return None;
    }
    for _ in 0..8 {
        let folder = temp.join(format!("haarness-{user}-{:08x}", rand::random::<u32>()));
        if std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&folder)
            .is_ok()
        {
            *own = Some(folder.clone());
            return Some(folder);
        }
    }
    None
}

/// The user's own temp folder's `haarness`.
#[cfg(not(unix))]
fn shared_base(make: bool) -> Option<PathBuf> {
    let temp = crate::paths::canonicalize(std::env::temp_dir()).ok()?;
    let folder = temp.join("haarness");
    private(&folder, make).then_some(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of this test's own, removed with everything in it.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("haarness-chat-temp-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&path).unwrap();
            let path = crate::paths::canonicalize(&path).unwrap();
            let chats = path.join("chats");
            make_folder(&chats).unwrap();
            Self(path)
        }
        fn chats(&self) -> PathBuf {
            self.0.join("chats")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A chat id whose last hex digits are `tail`.
    fn chat(head: &str, tail: &str) -> ThreadId {
        let hex = format!("{head}{tail}");
        let hex = format!("{}{hex}", "0".repeat(32 - hex.len()));
        ThreadId(uuid::Uuid::parse_str(&hex).unwrap())
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn a_chat_s_folder_is_made_once_named_by_the_end_of_its_id_for_its_owner_alone() {
        let scratch = Scratch::new();
        let chats = scratch.chats();
        let root = ThreadId::new();
        assert_eq!(
            existing_in(&chats, root),
            None,
            "nothing until a command runs"
        );
        let folder = folder_in(&chats, root).unwrap();
        let hex = root.0.simple().to_string();
        assert_eq!(folder, chats.join(&hex[24..]));
        assert!(folder.is_dir());
        #[cfg(unix)]
        assert_eq!(mode(&folder), 0o700);
        assert_eq!(
            std::fs::read_to_string(record(&chats, &hex[24..])).unwrap(),
            root.to_string()
        );
        // The same folder every time, its files kept.
        std::fs::write(folder.join("notes.txt"), "kept").unwrap();
        assert_eq!(folder_in(&chats, root), Some(folder.clone()));
        assert_eq!(existing_in(&chats, root), Some(folder.clone()));
        assert!(folder.join("notes.txt").is_file());
        assert_eq!(
            recorded_in(&chats),
            [Recorded {
                chat: root,
                chats: chats.clone(),
                name: hex[24..].to_owned(),
            }]
        );
    }

    #[test]
    fn a_chat_whose_name_another_holds_takes_a_longer_one() {
        let scratch = Scratch::new();
        let chats = scratch.chats();
        let first = chat("a1", "0000beef12345678");
        let second = chat("b2", "1111beef12345678");
        let mine = folder_in(&chats, first).unwrap();
        let theirs = folder_in(&chats, second).unwrap();
        assert_eq!(mine, chats.join("12345678"));
        assert_eq!(theirs, chats.join("beef12345678"));
        // A folder no record names is no chat's to take: the next name.
        let third = chat("c3", "2222feed00000000");
        make_folder(&chats.join("00000000")).unwrap();
        assert_eq!(folder_in(&chats, third), Some(chats.join("feed00000000")));
        assert!(!record(&chats, "00000000").exists());
        assert_eq!(existing_in(&chats, first), Some(mine));
        assert_eq!(existing_in(&chats, second), Some(theirs));
        // Taking one away leaves the other where it was.
        assert!(remove_in(&chats, first));
        assert_eq!(
            existing_in(&chats, second),
            Some(chats.join("beef12345678"))
        );
        assert_eq!(folder_in(&chats, second), Some(chats.join("beef12345678")));
    }

    #[test]
    fn removal_takes_only_the_chat_s_recorded_folder_and_never_follows_a_link() {
        let scratch = Scratch::new();
        let chats = scratch.chats();
        let outside = scratch.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "keep").unwrap();
        let root = ThreadId::new();
        let other = ThreadId::new();
        let folder = folder_in(&chats, root).unwrap();
        let others = folder_in(&chats, other).unwrap();
        std::fs::create_dir_all(folder.join("build/deep")).unwrap();
        std::fs::write(folder.join("build/deep/out.o"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::os::unix::fs::symlink(&outside, folder.join("link")).unwrap();
            std::os::unix::fs::symlink(&outside, folder.join("build/link")).unwrap();
            // A folder a command closed.
            std::fs::set_permissions(
                folder.join("build/deep"),
                std::fs::Permissions::from_mode(0o500),
            )
            .unwrap();
        }
        // A folder of the person's own that only has a chat's kind of name.
        let unrecorded = chats.join("cafe0000");
        std::fs::create_dir(&unrecorded).unwrap();
        assert!(remove_in(&chats, root));
        assert!(!folder.exists());
        assert!(!record(&chats, &folder.file_name().unwrap().to_string_lossy()).exists());
        assert!(outside.join("keep.txt").is_file(), "a link was followed");
        assert!(others.is_dir(), "another chat's folder went");
        assert!(unrecorded.is_dir());
        assert!(!remove_in(&chats, root), "nothing left to take");
        // A record that names another chat is not this one's.
        let stranger = chat("d4", "0000000099999999");
        std::fs::create_dir(chats.join("99999999")).unwrap();
        std::fs::write(record(&chats, "99999999"), other.to_string()).unwrap();
        assert!(!remove_in(&chats, stranger));
        assert!(chats.join("99999999").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_put_in_place_of_the_folder_is_replaced_without_touching_its_target() {
        let scratch = Scratch::new();
        let chats = scratch.chats();
        let outside = scratch.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "keep").unwrap();
        let root = ThreadId::new();
        let folder = folder_in(&chats, root).unwrap();
        std::fs::remove_dir(&folder).unwrap();
        std::os::unix::fs::symlink(&outside, &folder).unwrap();
        // Not looked up as the chat's folder while a link stands there.
        assert_eq!(existing_in(&chats, root), None);
        assert_eq!(folder_in(&chats, root), Some(folder.clone()));
        assert!(!std::fs::symlink_metadata(&folder).unwrap().is_symlink());
        assert!(outside.join("keep.txt").is_file());
        // Taken away again, the link is the entry itself.
        std::fs::remove_dir(&folder).unwrap();
        std::os::unix::fs::symlink(&outside, &folder).unwrap();
        assert!(remove_in(&chats, root));
        assert!(outside.join("keep.txt").is_file());
        assert!(std::fs::symlink_metadata(&folder).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_counts_as_untouched_from_the_last_change_in_it_or_its_last_use() {
        let scratch = Scratch::new();
        let chats = scratch.chats();
        let root = ThreadId::new();
        let folder = folder_in(&chats, root).unwrap();
        std::fs::create_dir(folder.join("sub")).unwrap();
        std::fs::write(folder.join("sub/file"), "x").unwrap();
        let old = SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
        let age = |path: &Path, time: SystemTime| {
            std::fs::File::options()
                .write(true)
                .open(path)
                .or_else(|_| std::fs::File::open(path))
                .unwrap()
                .set_modified(time)
                .unwrap();
        };
        let recorded = || recorded_in(&chats).remove(0);
        // Just made and used.
        assert!(recorded().untouched(SystemTime::now()) < Duration::from_secs(60));
        for path in [
            folder.join("sub/file"),
            folder.join("sub"),
            folder.clone(),
            record(&chats, &recorded().name),
        ] {
            age(&path, old);
        }
        let untouched = recorded().untouched(SystemTime::now());
        assert!(untouched >= IDLE_LIMIT, "{untouched:?}");
        // A file written lately in a folder below.
        std::fs::write(folder.join("sub/file"), "y").unwrap();
        assert!(recorded().untouched(SystemTime::now()) < Duration::from_secs(60));
        age(&folder.join("sub/file"), old);
        age(&folder.join("sub"), old);
        // A command of the chat starts.
        assert_eq!(folder_in(&chats, root), Some(folder));
        assert!(recorded().untouched(SystemTime::now()) < Duration::from_secs(60));
    }

    /// The folder's path leaves room for the sockets programs make in
    /// `TMPDIR` below macOS's limit of 104 bytes (with the closing zero),
    /// for its longest name too: Python's `multiprocessing` makes
    /// `pymp-<8>/listener-<8>` there.
    #[cfg(unix)]
    #[test]
    fn a_socket_python_makes_in_the_folder_fits_the_system_s_limit() {
        const SOCKET: &str = "pymp-k3j4h5g6/listener-a1b2c3d4";
        let temp = crate::paths::canonicalize("/tmp").unwrap();
        let user = nix::unistd::geteuid().as_raw();
        let hex = ThreadId::new().0.simple().to_string();
        for (base, name) in [
            (format!("haarness-{user}"), &hex[24..]),
            (format!("haarness-{user}-ffffffff"), &hex[12..]),
            // The longest a user id and a name can be.
            (format!("haarness-{}-ffffffff", u32::MAX), &hex[12..]),
        ] {
            let path = temp.join(base).join("chats").join(name).join(SOCKET);
            assert!(path.as_os_str().len() < 104, "{}", path.display());
        }
        // A real socket at a path that long, in a short folder of the
        // test's own in /tmp: the shape it would have.
        let own = temp.join(format!(
            "hct-{}",
            &uuid::Uuid::now_v7().simple().to_string()[24..]
        ));
        std::fs::create_dir(&own).unwrap();
        let typical = temp
            .join(format!("haarness-{user}"))
            .join("chats")
            .join(&hex[24..]);
        let pad = typical.as_os_str().len() - own.as_os_str().len() - 1;
        let folder = own.join("x".repeat(pad)).join("pymp-k3j4h5g6");
        assert_eq!(
            folder.as_os_str().len(),
            typical.join("pymp-k3j4h5g6").as_os_str().len()
        );
        std::fs::create_dir_all(&folder).unwrap();
        let bound = std::os::unix::net::UnixListener::bind(folder.join("listener-a1b2c3d4"));
        let _ = std::fs::remove_dir_all(&own);
        bound.unwrap();
    }
}
