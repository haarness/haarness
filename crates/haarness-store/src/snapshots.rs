//! Content-addressed workspace snapshots in SQLite.
//!
//! A snapshot is a complete manifest of the files that Haarness is willing to
//! manage. Hidden source files are included, gitignore rules are respected,
//! and `.git` is never traversed. Large files remain in the manifest as
//! unrestorable `omitted` entries instead of silently disappearing. So do
//! entries that cannot be read (a root-owned directory, a file that keeps
//! changing while it is read): one such path must not fail every capture,
//! and no restore ever writes or deletes it.
//!
//! The database is shared with the chats' own store, so a capture never
//! holds its write lock for long: entries are read, hashed and compressed
//! outside any transaction, and written a batch per short transaction. The
//! `snapshots` row comes last, so a capture is invisible until it is
//! complete, and one that never completes (a crash) is removed by retention.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::Utc;
use haarness_core::{CoreError, CoreResult, SnapshotInfo, SnapshotStore};
use haarness_protocol::{FileDiff, FileDiffStatus, ThreadId};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_DIFF_BYTES: usize = 10 * 1024 * 1024;
/// The hash of an omitted entry whose content could not be read. It is not
/// a SHA-256, so it never matches a live file and no blob is stored for it.
const UNREADABLE: &str = "unreadable";
/// Paths one store warns about before it stays quiet, whatever the tree holds.
const MAX_WARNED_PATHS: usize = 256;
/// The most entries, and file content, one capture writes per transaction:
/// a chat's write waits for one batch at most, however large the tree.
const BATCH: BatchLimits = BatchLimits {
    entries: 1024,
    bytes: 8 * 1024 * 1024,
};
/// How long a capture's manifest rows may stand without the `snapshots` row
/// that completes them before retention takes them for a crashed capture's.
/// Measured from the time in the capture's v7 id, so a retention pass in
/// another process sharing the database agrees on it; a day, so a capture
/// across a laptop's sleep or a clock jump is not taken. This process's own
/// captures are never taken (see `CAPTURING`).
const ABANDONED_AFTER_MS: i64 = 24 * 60 * 60 * 1000;
const BLOB_STORED: &str = "SELECT EXISTS(SELECT 1 FROM snapshot_blobs WHERE hash=?1)";

#[derive(Debug, Clone, Copy)]
struct BatchLimits {
    entries: usize,
    bytes: usize,
}

/// Where a capture is: after one batch was checked against the stored blobs
/// (and its new content compressed), and after it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureStep {
    Checked,
    Written,
}

/// What a capture wrote before its `snapshots` row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Written {
    /// Manifest rows.
    files: u64,
    /// Blobs compressed: content no blob held yet.
    compressed: u64,
    /// Blobs it added to the store, each with the rows that name it.
    blobs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    SymlinkFile,
    SymlinkDirectory,
    Omitted,
}

impl EntryKind {
    fn database_name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::SymlinkFile => "symlink_file",
            Self::SymlinkDirectory => "symlink_directory",
            Self::Omitted => "omitted",
        }
    }

    fn from_database(value: &str) -> CoreResult<Self> {
        match value {
            "file" => Ok(Self::File),
            "symlink_file" => Ok(Self::SymlinkFile),
            "symlink_directory" => Ok(Self::SymlinkDirectory),
            "omitted" => Ok(Self::Omitted),
            other => Err(CoreError::Storage(format!(
                "unknown snapshot entry kind {other:?}"
            ))),
        }
    }

    fn is_symlink(self) -> bool {
        matches!(self, Self::SymlinkFile | Self::SymlinkDirectory)
    }

    fn has_blob(self) -> bool {
        !matches!(self, Self::Omitted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestEntry {
    path: String,
    hash: String,
    kind: EntryKind,
    mode: Option<u32>,
    size: u64,
}

impl ManifestEntry {
    /// An entry whose content could not be read. It compares equal across
    /// snapshots, so a path that stays unreadable is never a change.
    fn unreadable(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            hash: UNREADABLE.to_owned(),
            kind: EntryKind::Omitted,
            mode: None,
            size: 0,
        }
    }

    fn is_unreadable(&self) -> bool {
        self.kind == EntryKind::Omitted && self.hash == UNREADABLE
    }
}

#[derive(Debug)]
struct SourcePath {
    absolute: PathBuf,
    relative: String,
    /// Why the walk could not read this entry (a directory it could not
    /// list); it is recorded without being scanned.
    unreadable: Option<String>,
}

#[derive(Debug)]
struct ScannedEntry {
    manifest: ManifestEntry,
    data: Option<Vec<u8>>,
}

/// One walked entry, read for a snapshot.
#[derive(Debug)]
enum Scan {
    Entry(ScannedEntry),
    /// Deleted after the walk listed it.
    Gone,
    /// Unreadable, or still changing when read a second time.
    Unreadable(CoreError),
}

/// Read one entry, and once more if that fails: a file written to while it
/// was read usually settles, and one replaced by a rename is back at once.
fn scan_settled(absolute: &Path, mut scan: impl FnMut() -> CoreResult<ScannedEntry>) -> Scan {
    let first = match scan() {
        Ok(entry) => return Scan::Entry(entry),
        Err(error) => error,
    };
    tracing::debug!(path = %absolute.display(), error = %first, "snapshot entry unreadable; retrying once");
    match scan() {
        Ok(entry) => Scan::Entry(entry),
        Err(_)
            if fs::symlink_metadata(absolute)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Scan::Gone
        }
        Err(error) => Scan::Unreadable(error),
    }
}

/// The path a walk error is about, if it names one.
fn walk_error_path(error: &ignore::Error) -> Option<&Path> {
    match error {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            walk_error_path(err)
        }
        _ => None,
    }
}

#[derive(Debug)]
struct PreparedEntry {
    manifest: ManifestEntry,
    data: Vec<u8>,
}

#[derive(Debug)]
struct PreparedChange {
    path: String,
    desired: Option<ManifestEntry>,
}

#[derive(Clone)]
pub struct SqliteSnapshotStore {
    // Its own connection to the shared database file: large blob writes never
    // contend with the hot thread/item path.
    connection: Arc<Mutex<Connection>>,
    workspace: PathBuf,
    /// Unreadable paths already logged at WARN; every capture meets them again.
    warned: Arc<Mutex<BTreeSet<String>>>,
    /// Whether ignore files in the folders above `workspace` count, as in a
    /// project; a folder in the chat home reads none
    /// ([`SqliteSnapshotFolders::open_in_chat_home`]).
    outer_ignores: bool,
}

impl SqliteSnapshotStore {
    pub fn open(database: impl AsRef<Path>, workspace: PathBuf) -> CoreResult<Self> {
        let workspace = snapshot_root(workspace)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(open_connection(database)?)),
            workspace,
            warned: Arc::default(),
            outer_ignores: true,
        })
    }

    /// The stores of chats' own folders, on this store's connection.
    #[must_use]
    pub fn folders(&self) -> SqliteSnapshotFolders {
        // Already open, so no database path is needed to open it.
        SqliteSnapshotFolders::with_connection(PathBuf::new(), Some(Arc::clone(&self.connection)))
    }

    /// Walk the complete managed tree. `hidden(false)` means "do not filter
    /// hidden entries" in the `ignore` crate; gitignore rules remain enabled.
    /// A directory below the root that cannot be listed becomes one
    /// unreadable entry; the root itself must be readable.
    fn source_paths(&self) -> CoreResult<Vec<SourcePath>> {
        let mut builder = ignore::WalkBuilder::new(&self.workspace);
        builder
            .hidden(false)
            .follow_links(false)
            .parents(self.outer_ignores);
        builder.filter_entry(|entry| {
            entry.depth() == 0
                || (entry.file_name() != OsStr::new(".git")
                    && !is_restore_scratch(entry.file_name()))
        });

        let mut sources = Vec::new();
        for result in builder.build() {
            let entry = match result {
                Ok(entry) => entry,
                Err(error) => {
                    let relative = walk_error_path(&error)
                        .and_then(|path| path.strip_prefix(&self.workspace).ok())
                        .filter(|relative| !relative.as_os_str().is_empty());
                    let Some(relative) = relative else {
                        return Err(CoreError::Storage(format!(
                            "cannot walk snapshot workspace: {error}"
                        )));
                    };
                    let relative = encode_relative_path(relative)?;
                    let _ = decode_relative_path(&relative)?;
                    sources.push(SourcePath {
                        absolute: self.workspace.join(&relative),
                        relative,
                        unreadable: Some(error.to_string()),
                    });
                    continue;
                }
            };
            if entry.depth() == 0 {
                continue;
            }
            let file_type = entry.file_type().ok_or_else(|| {
                CoreError::Storage(format!(
                    "cannot determine file type for {}",
                    entry.path().display()
                ))
            })?;
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }
            let relative = entry.path().strip_prefix(&self.workspace).map_err(|_| {
                CoreError::Invalid(format!(
                    "snapshot path escaped workspace: {}",
                    entry.path().display()
                ))
            })?;
            let relative = encode_relative_path(relative)?;
            // Decode too so capture and restore share the exact same safety
            // contract (including the hard `.git` exclusion).
            let _ = decode_relative_path(&relative)?;
            sources.push(SourcePath {
                absolute: entry.into_path(),
                relative,
                unreadable: None,
            });
        }
        sources.sort_by(|left, right| left.relative.cmp(&right.relative));
        // One path can be listed and then fail; the manifest holds it once.
        sources.dedup_by(|later, earlier| later.relative == earlier.relative);
        Ok(sources)
    }

    /// Read one walked entry for a manifest. `None` when it is gone; an
    /// entry that cannot be read is recorded as unreadable, so one busy or
    /// root-owned path never fails a capture.
    fn capture_entry(&self, source: &SourcePath, include_data: bool) -> Option<ScannedEntry> {
        let error = match &source.unreadable {
            Some(error) => error.clone(),
            None => {
                match scan_settled(&source.absolute, || self.scan_entry(source, include_data)) {
                    Scan::Entry(entry) => return Some(entry),
                    Scan::Gone => return None,
                    Scan::Unreadable(error) => error.to_string(),
                }
            }
        };
        tracing::debug!(path = %source.relative, %error, "snapshot cannot read entry");
        let mut warned = self.warned.lock();
        if warned.len() < MAX_WARNED_PATHS && warned.insert(source.relative.clone()) {
            tracing::warn!(
                path = %source.relative,
                %error,
                "snapshots cannot read this path; recording it as omitted, and undo will not restore it"
            );
        }
        Some(ScannedEntry {
            manifest: ManifestEntry::unreadable(&source.relative),
            data: None,
        })
    }

    fn scan_entry(&self, source: &SourcePath, include_data: bool) -> CoreResult<ScannedEntry> {
        ensure_safe_ancestors(&self.workspace, &source.relative)?;
        let metadata = fs::symlink_metadata(&source.absolute).map_err(io_error)?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&source.absolute).map_err(io_error)?;
            let data = os_string_bytes(target.into_os_string());
            let kind = if self.symlink_points_to_managed_directory(&source.absolute)? {
                EntryKind::SymlinkDirectory
            } else {
                EntryKind::SymlinkFile
            };
            return Ok(ScannedEntry {
                manifest: ManifestEntry {
                    path: source.relative.clone(),
                    hash: hex_digest(&data),
                    kind,
                    mode: None,
                    size: u64::try_from(data.len()).unwrap_or(u64::MAX),
                },
                data: include_data.then_some(data),
            });
        }
        if !metadata.is_file() {
            return Err(CoreError::Conflict(format!(
                "snapshot entry changed type while capturing: {}",
                source.relative
            )));
        }

        let size = metadata.len();
        let mode = file_mode(&metadata);
        if size > MAX_FILE_BYTES {
            let (hash, actual_size) = hash_file(&source.absolute)?;
            let after = fs::symlink_metadata(&source.absolute).map_err(io_error)?;
            if !after.is_file() || after.len() != actual_size {
                return Err(CoreError::Conflict(format!(
                    "snapshot entry changed while capturing: {}",
                    source.relative
                )));
            }
            return Ok(ScannedEntry {
                manifest: ManifestEntry {
                    path: source.relative.clone(),
                    hash,
                    kind: EntryKind::Omitted,
                    mode,
                    size: actual_size,
                },
                data: None,
            });
        }

        let data = fs::read(&source.absolute).map_err(io_error)?;
        let after = fs::symlink_metadata(&source.absolute).map_err(io_error)?;
        if !after.is_file()
            || after.len() != u64::try_from(data.len()).unwrap_or(u64::MAX)
            || file_mode(&after) != mode
        {
            return Err(CoreError::Conflict(format!(
                "snapshot entry changed while capturing: {}",
                source.relative
            )));
        }
        Ok(ScannedEntry {
            manifest: ManifestEntry {
                path: source.relative.clone(),
                hash: hex_digest(&data),
                kind: EntryKind::File,
                mode,
                size: u64::try_from(data.len()).unwrap_or(u64::MAX),
            },
            data: include_data.then_some(data),
        })
    }

    /// Determine the Windows symlink flavor without following a link outside
    /// the managed root. Unix does not need the distinction to recreate it,
    /// but storing it keeps manifests portable.
    fn symlink_points_to_managed_directory(&self, link: &Path) -> CoreResult<bool> {
        let target = fs::read_link(link).map_err(io_error)?;
        let candidate = if target.is_absolute() {
            target
        } else {
            link.parent().unwrap_or(&self.workspace).join(target)
        };
        let Ok(canonical) = candidate.canonicalize() else {
            return Ok(false);
        };
        if !canonical.starts_with(&self.workspace) {
            return Ok(false);
        }
        Ok(canonical.is_dir())
    }

    fn scan_manifest(&self) -> CoreResult<BTreeMap<String, ManifestEntry>> {
        let mut manifest = BTreeMap::new();
        for source in self.source_paths()? {
            if let Some(entry) = self.capture_entry(&source, false) {
                manifest.insert(entry.manifest.path.clone(), entry.manifest);
            }
        }
        Ok(manifest)
    }

    fn snapshot_exists(&self, snapshot_id: &str) -> CoreResult<bool> {
        self.connection
            .lock()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM snapshots WHERE id=?1)",
                [snapshot_id],
                |row| row.get(0),
            )
            .map_err(storage)
    }

    fn load_manifest(&self, snapshot_id: &str) -> CoreResult<BTreeMap<String, ManifestEntry>> {
        let exists = self.snapshot_exists(snapshot_id)?;
        if !exists {
            return Err(CoreError::NotFound(format!("snapshot {snapshot_id}")));
        }
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare(
                "SELECT f.path,f.hash,f.kind,f.mode,COALESCE(f.size,b.size,0) \
                 FROM snapshot_files f \
                 LEFT JOIN snapshot_blobs b ON b.hash=f.hash \
                 WHERE f.snapshot_id=?1 ORDER BY f.path",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map([snapshot_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(storage)?;

        let mut manifest = BTreeMap::new();
        for row in rows {
            let (path, hash, kind, mode, size) = row.map_err(storage)?;
            let _ = decode_relative_path(&path)?;
            let entry = ManifestEntry {
                path: path.clone(),
                hash,
                kind: EntryKind::from_database(&kind)?,
                mode: mode.and_then(|value| u32::try_from(value).ok()),
                size: u64::try_from(size)
                    .map_err(|_| CoreError::Storage(format!("invalid snapshot size for {path}")))?,
            };
            if manifest.insert(path.clone(), entry).is_some() {
                return Err(CoreError::Storage(format!(
                    "duplicate path in snapshot manifest: {path}"
                )));
            }
        }
        Ok(manifest)
    }

    fn load_blob(&self, entry: &ManifestEntry) -> CoreResult<Vec<u8>> {
        if entry.is_unreadable() {
            return Err(CoreError::Conflict(format!(
                "snapshot entry {} could not be read when it was captured",
                entry.path
            )));
        }
        if !entry.kind.has_blob() {
            return Err(CoreError::Conflict(format!(
                "snapshot entry {} was omitted because it exceeds {} bytes",
                entry.path, MAX_FILE_BYTES
            )));
        }
        let row: Option<(i64, Vec<u8>)> = self
            .connection
            .lock()
            .query_row(
                "SELECT size,data FROM snapshot_blobs WHERE hash=?1",
                [&entry.hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        let (stored_size, compressed) =
            row.ok_or_else(|| CoreError::Storage(format!("missing snapshot blob {}", entry.hash)))?;
        let mut decoder = flate2::read::ZlibDecoder::new(compressed.as_slice());
        let mut data = Vec::new();
        decoder.read_to_end(&mut data).map_err(io_error)?;
        let actual_size = u64::try_from(data.len()).unwrap_or(u64::MAX);
        if u64::try_from(stored_size).ok() != Some(actual_size)
            || actual_size != entry.size
            || hex_digest(&data) != entry.hash
        {
            return Err(CoreError::Storage(format!(
                "corrupt snapshot blob {} for {}",
                entry.hash, entry.path
            )));
        }
        Ok(data)
    }

    fn prepare_entry(&self, entry: &ManifestEntry) -> CoreResult<PreparedEntry> {
        Ok(PreparedEntry {
            manifest: entry.clone(),
            data: self.load_blob(entry)?,
        })
    }

    fn live_matches(&self, expected: Option<&ManifestEntry>) -> CoreResult<bool> {
        let Some(expected) = expected else {
            return Ok(false);
        };
        let target = safe_target(&self.workspace, &expected.path)?;
        ensure_safe_ancestors(&self.workspace, &expected.path)?;
        let Some(actual) = scan_live_entry(&target, &expected.path, &self.workspace)? else {
            return Ok(false);
        };
        Ok(entries_match_live(expected, &actual))
    }

    fn diff_manifests(
        &self,
        before: &BTreeMap<String, ManifestEntry>,
        after: &BTreeMap<String, ManifestEntry>,
        after_is_live: bool,
        summary: bool,
        selected: Option<&str>,
    ) -> CoreResult<Vec<FileDiff>> {
        let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
        let mut remaining = MAX_DIFF_BYTES;
        let mut files = Vec::new();
        for path in paths {
            if selected.is_some_and(|selected| selected != path) {
                continue;
            }
            let left = before.get(path);
            let right = after.get(path);
            if left == right {
                continue;
            }
            let status = match (left, right) {
                (None, Some(_)) => FileDiffStatus::Added,
                (Some(_), None) => FileDiffStatus::Deleted,
                _ => FileDiffStatus::Modified,
            };

            let left_bytes = match left {
                Some(entry) if entry.kind == EntryKind::Omitted => None,
                Some(entry) => Some(self.load_blob(entry)?),
                None => Some(Vec::new()),
            };
            let right_bytes = match right {
                Some(entry) if entry.kind == EntryKind::Omitted => None,
                Some(entry) if after_is_live => {
                    let target = safe_target(&self.workspace, path)?;
                    read_live_data(&target, entry)?
                }
                Some(entry) => Some(self.load_blob(entry)?),
                None => Some(Vec::new()),
            };

            let omitted = left.is_some_and(|entry| entry.kind == EntryKind::Omitted)
                || right.is_some_and(|entry| entry.kind == EntryKind::Omitted);
            let binary = left_bytes.as_deref().is_some_and(is_binary)
                || right_bytes.as_deref().is_some_and(is_binary);
            let mut truncated = omitted;
            let (additions, deletions) = match (&left_bytes, &right_bytes) {
                (Some(left), Some(right)) if !binary => line_counts(left, right),
                _ => (0, 0),
            };

            let body_size =
                left_bytes.as_ref().map_or(0, Vec::len) + right_bytes.as_ref().map_or(0, Vec::len);
            let include_body = !summary && !binary && !omitted && body_size <= remaining;
            let (before_body, after_body) = if include_body {
                remaining -= body_size;
                (
                    match left {
                        Some(_) => left_bytes
                            .as_deref()
                            .map(|value| String::from_utf8_lossy(value).into_owned()),
                        None => None,
                    },
                    match right {
                        Some(_) => right_bytes
                            .as_deref()
                            .map(|value| String::from_utf8_lossy(value).into_owned()),
                        None => None,
                    },
                )
            } else {
                if !summary && !binary {
                    truncated = true;
                }
                (None, None)
            };

            files.push(FileDiff {
                file: path.clone(),
                status,
                additions,
                deletions,
                patch: None,
                before: before_body,
                after: after_body,
                binary,
                truncated,
            });
        }
        Ok(files)
    }

    /// Drop snapshots the workspace can no longer reach, then the blobs that
    /// were only theirs.
    ///
    /// Without this the blob store is append-only: every turn that touches a
    /// file adds another copy of it forever. Three things are always kept —
    /// anything newer than `retention`, the most recent `keep_per_thread`
    /// checkpoints of each thread (so undo still has somewhere to go), and
    /// whatever a pending undo is holding as its backup or base. What
    /// unfinished captures left goes too (see [`Self::sweep`]).
    pub fn prune(
        &self,
        retention: chrono::Duration,
        keep_per_thread: usize,
    ) -> CoreResult<PruneReport> {
        prune(&self.connection, Some((retention, keep_per_thread)))
    }

    /// The retention pass that keeps every checkpoint: it removes only the
    /// rows of abandoned captures (see [`ABANDONED_AFTER_MS`]) and the blobs
    /// no manifest row names any more. A capture still being written keeps
    /// its rows and their blobs.
    pub fn sweep(&self) -> CoreResult<PruneReport> {
        prune(&self.connection, None)
    }
}

/// The captures this process is writing. A retention pass of this process
/// never takes one for abandoned, however long it runs (across a laptop's
/// sleep, or a clock that jumped forward).
static CAPTURING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// The databases in which a capture of this process failed and left blobs
/// no manifest row names: the next retention pass there removes them,
/// though it has nothing else to remove.
static ORPHANED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Capture `id` is being written, until this is dropped.
struct Capturing(String);

impl Capturing {
    fn start(id: &str) -> Self {
        CAPTURING.lock().insert(id.to_owned());
        Self(id.to_owned())
    }
}

impl Drop for Capturing {
    fn drop(&mut self) {
        CAPTURING.lock().remove(&self.0);
    }
}

fn database_key(connection: &Connection) -> Option<String> {
    connection
        .path()
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
}

/// `expiry` is the retention and the checkpoints kept per thread; without
/// it every checkpoint is kept.
fn prune(
    connection: &Mutex<Connection>,
    expiry: Option<(chrono::Duration, usize)>,
) -> CoreResult<PruneReport> {
    let connection = connection.lock();
    let doomed = match expiry {
        Some((retention, keep_per_thread)) => {
            expired_snapshots(&connection, retention, keep_per_thread)?
        }
        None => Vec::new(),
    };
    let abandoned = abandoned_captures(&connection)?;
    reclaim(&connection, &doomed, &abandoned)
}

fn expired_snapshots(
    connection: &Connection,
    retention: chrono::Duration,
    keep_per_thread: usize,
) -> CoreResult<Vec<String>> {
    let cutoff = Utc::now() - retention;
    let mut protected: BTreeSet<String> = BTreeSet::new();
    // A pending undo is live state, never garbage, whatever its age.
    let mut pending = connection
        .prepare(
            "SELECT revert_snapshot, revert_base_snapshot FROM threads \
             WHERE revert_snapshot IS NOT NULL OR revert_base_snapshot IS NOT NULL",
        )
        .map_err(storage)?;
    let rows = pending
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .map_err(storage)?;
    for row in rows {
        let (backup, base) = row.map_err(storage)?;
        protected.extend(backup);
        protected.extend(base);
    }
    drop(pending);

    let mut all = connection
        .prepare(
            "SELECT id, thread_id, created_at FROM snapshots ORDER BY created_at DESC, id DESC",
        )
        .map_err(storage)?;
    let rows = all
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(storage)?;
    let mut kept_per_thread: BTreeMap<String, usize> = BTreeMap::new();
    let mut doomed: Vec<String> = Vec::new();
    for row in rows {
        let (id, thread, created_at) = row.map_err(storage)?;
        if protected.contains(&id) {
            continue;
        }
        let kept = kept_per_thread.entry(thread).or_default();
        if *kept < keep_per_thread {
            *kept += 1;
            continue;
        }
        // An unparseable timestamp is treated as recent: refusing to
        // delete is always the safer side of this decision.
        let recent = chrono::DateTime::parse_from_rfc3339(&created_at)
            .map(|created| created.with_timezone(&Utc) >= cutoff)
            .unwrap_or(true);
        if recent {
            continue;
        }
        doomed.push(id);
    }
    Ok(doomed)
}

/// Remove the `doomed` snapshots and the rows of the `abandoned` captures,
/// then every blob that no manifest row names any more.
fn reclaim(
    connection: &Connection,
    doomed: &[String],
    abandoned: &[String],
) -> CoreResult<PruneReport> {
    let database = database_key(connection);
    let orphaned = database
        .as_ref()
        .is_some_and(|database| ORPHANED.lock().remove(database));
    if doomed.is_empty() && abandoned.is_empty() && !orphaned {
        return Ok(PruneReport::default());
    }
    let reclaimed = reclaim_rows(connection, doomed, abandoned);
    if reclaimed.is_err()
        && orphaned
        && let Some(database) = database
    {
        ORPHANED.lock().insert(database);
    }
    reclaimed
}

fn reclaim_rows(
    connection: &Connection,
    doomed: &[String],
    abandoned: &[String],
) -> CoreResult<PruneReport> {
    let transaction = connection.unchecked_transaction().map_err(storage)?;
    for id in doomed {
        transaction
            .execute("DELETE FROM snapshot_files WHERE snapshot_id=?1", [id])
            .map_err(storage)?;
        transaction
            .execute("DELETE FROM snapshots WHERE id=?1", [id])
            .map_err(storage)?;
    }
    let mut swept = 0;
    for id in abandoned {
        // A capture that wrote its `snapshots` row since it was listed is
        // a snapshot now, and keeps its rows.
        let removed = transaction
            .execute(
                "DELETE FROM snapshot_files WHERE snapshot_id=?1 \
                 AND NOT EXISTS(SELECT 1 FROM snapshots WHERE id=?1)",
                [id],
            )
            .map_err(storage)?;
        swept += usize::from(removed > 0);
    }
    // Blobs are content-addressed and shared, so they only go once the
    // last manifest that named them is gone. The rows of a capture still
    // being written name theirs too, so they stay. This scans every
    // manifest row under the write lock, which on a very large database
    // can outlast another connection's busy timeout; a sweep in short
    // batches over an index on `snapshot_files(hash)` would not (follow-up).
    let blobs = transaction
        .execute(
            "DELETE FROM snapshot_blobs WHERE hash NOT IN (SELECT hash FROM snapshot_files)",
            [],
        )
        .map_err(storage)?;
    transaction.commit().map_err(storage)?;
    Ok(PruneReport {
        snapshots: doomed.len(),
        blobs,
        abandoned: swept,
    })
}

/// Captures that wrote manifest rows but no `snapshots` row, that this
/// process is not writing, and that started more than
/// [`ABANDONED_AFTER_MS`] ago: a crash or a failure stopped them. Any id
/// that is not a v7 UUID is kept.
fn abandoned_captures(connection: &Connection) -> CoreResult<Vec<String>> {
    let cutoff = Utc::now().timestamp_millis() - ABANDONED_AFTER_MS;
    // One seek per manifest, rather than a scan of every manifest row.
    let mut next = connection
        .prepare(
            "SELECT snapshot_id FROM snapshot_files WHERE snapshot_id>?1 \
             ORDER BY snapshot_id LIMIT 1",
        )
        .map_err(storage)?;
    let mut published = connection
        .prepare("SELECT EXISTS(SELECT 1 FROM snapshots WHERE id=?1)")
        .map_err(storage)?;
    let mut abandoned = Vec::new();
    let mut after = String::new();
    while let Some(id) = next
        .query_row([&after], |row| row.get::<_, String>(0))
        .optional()
        .map_err(storage)?
    {
        if started_before(&id, cutoff)
            && !CAPTURING.lock().contains(&id)
            && !published
                .query_row([&id], |row| row.get::<_, bool>(0))
                .map_err(storage)?
        {
            abandoned.push(id.clone());
        }
        after = id;
    }
    Ok(abandoned)
}

/// Whether a capture id is a v7 UUID whose time is before `cutoff_ms`.
fn started_before(id: &str, cutoff_ms: i64) -> bool {
    uuid::Uuid::parse_str(id)
        .ok()
        .filter(|id| id.get_version_num() == 7)
        .and_then(|id| id.get_timestamp())
        .is_some_and(|time| {
            let (seconds, nanos) = time.to_unix();
            i64::try_from(seconds).is_ok_and(|seconds| {
                seconds.saturating_mul(1000) + i64::from(nanos / 1_000_000) < cutoff_ms
            })
        })
}

fn open_connection(database: impl AsRef<Path>) -> CoreResult<Connection> {
    let connection = Connection::open(database).map_err(storage)?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(storage)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(storage)?;
    Ok(connection)
}

/// The canonical folder a store manages: a folder, and never the
/// operating-system home folder.
fn snapshot_root(workspace: PathBuf) -> CoreResult<PathBuf> {
    let workspace = workspace.canonicalize().map_err(|error| {
        CoreError::Storage(format!(
            "cannot canonicalize snapshot workspace {}: {error}",
            workspace.display()
        ))
    })?;
    if !workspace.is_dir() {
        return Err(CoreError::Invalid(format!(
            "snapshot workspace is not a directory: {}",
            workspace.display()
        )));
    }
    let configured_homes = [std::env::var_os("HOME"), std::env::var_os("USERPROFILE")]
        .into_iter()
        .flatten();
    if matches_configured_home(&workspace, configured_homes) {
        return Err(CoreError::Invalid(format!(
            "refusing to snapshot the operating-system home directory: {}",
            workspace.display()
        )));
    }
    Ok(workspace)
}

/// The snapshot stores of chats that work in a folder of their own, all on
/// one connection to the session database: each store captures and restores
/// only inside its folder. A server none of whose chats has a folder never
/// opens that connection: it is opened when the first folder is.
#[derive(Clone)]
pub struct SqliteSnapshotFolders {
    inner: Arc<FolderStores>,
}

struct FolderStores {
    database: PathBuf,
    connection: Mutex<Option<Arc<Mutex<Connection>>>>,
    /// One store per folder, so a folder's unreadable paths are warned
    /// about once and it is looked up once.
    stores: Mutex<BTreeMap<PathBuf, Arc<SqliteSnapshotStore>>>,
    /// See [`SqliteSnapshotStore::outer_ignores`].
    outer_ignores: bool,
}

impl SqliteSnapshotFolders {
    /// For a server whose project folder itself is not checkpointed: the
    /// database is opened when a chat's folder first is.
    #[must_use]
    pub fn open(database: impl AsRef<Path>) -> Self {
        Self::with_connection(database.as_ref().to_path_buf(), None)
    }

    /// For the chat home, whose chats' folders stand alone: no ignore file
    /// in the folders above one counts, so a repository the chat home lies
    /// in (a home folder kept in Git) never keeps a chat's files out of its
    /// checkpoints. A folder's own ignore files still count.
    #[must_use]
    pub fn open_in_chat_home(database: impl AsRef<Path>) -> Self {
        Self {
            inner: Arc::new(FolderStores {
                database: database.as_ref().to_path_buf(),
                connection: Mutex::default(),
                stores: Mutex::default(),
                outer_ignores: false,
            }),
        }
    }

    fn with_connection(database: PathBuf, connection: Option<Arc<Mutex<Connection>>>) -> Self {
        Self {
            inner: Arc::new(FolderStores {
                database,
                connection: Mutex::new(connection),
                stores: Mutex::default(),
                outer_ignores: true,
            }),
        }
    }

    fn connection(&self) -> CoreResult<Arc<Mutex<Connection>>> {
        let mut connection = self.inner.connection.lock();
        if let Some(open) = connection.as_ref() {
            return Ok(Arc::clone(open));
        }
        let opened = Arc::new(Mutex::new(open_connection(&self.inner.database)?));
        *connection = Some(Arc::clone(&opened));
        Ok(opened)
    }

    /// The database's retention pass (see [`SqliteSnapshotStore::prune`]):
    /// every checkpoint of the database, whichever folder it holds. Nothing
    /// to do while no folder has been opened.
    pub fn prune(
        &self,
        retention: chrono::Duration,
        keep_per_thread: usize,
    ) -> CoreResult<PruneReport> {
        let connection = self.inner.connection.lock().clone();
        match connection {
            Some(connection) => prune(&connection, Some((retention, keep_per_thread))),
            None => Ok(PruneReport::default()),
        }
    }

    /// The pass that keeps every checkpoint (see
    /// [`SqliteSnapshotStore::sweep`]). Nothing to do while no folder has
    /// been opened.
    pub fn sweep(&self) -> CoreResult<PruneReport> {
        let connection = self.inner.connection.lock().clone();
        match connection {
            Some(connection) => prune(&connection, None),
            None => Ok(PruneReport::default()),
        }
    }
}

impl haarness_core::SnapshotFolders for SqliteSnapshotFolders {
    fn open(&self, folder: &Path) -> CoreResult<Arc<dyn SnapshotStore>> {
        if let Some(store) = self.inner.stores.lock().get(folder) {
            return Ok(Arc::clone(store) as Arc<dyn SnapshotStore>);
        }
        let store = Arc::new(SqliteSnapshotStore {
            connection: self.connection()?,
            workspace: snapshot_root(folder.to_path_buf())?,
            warned: Arc::default(),
            outer_ignores: self.inner.outer_ignores,
        });
        self.inner
            .stores
            .lock()
            .insert(folder.to_path_buf(), Arc::clone(&store));
        Ok(store)
    }
}

/// What one retention pass reclaimed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub snapshots: usize,
    pub blobs: usize,
    /// Captures a crash or a failure left unfinished.
    pub abandoned: usize,
}

impl SqliteSnapshotStore {
    /// Run `work` on the blocking pool: captures and diffs read the whole
    /// tree or its manifests, which must never stall an async worker.
    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Self) -> CoreResult<T> + Send + 'static,
    ) -> CoreResult<T> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || work(&store))
            .await
            .map_err(|error| CoreError::Storage(format!("snapshot worker failed: {error}")))?
    }

    fn capture_sync(&self, thread: ThreadId) -> CoreResult<SnapshotInfo> {
        let id = uuid::Uuid::now_v7().to_string();
        self.capture_as(&id, thread, BATCH, |_| Ok(()))
    }

    /// Capture the tree as snapshot `id`. What a failed capture wrote is
    /// removed again.
    fn capture_as(
        &self,
        id: &str,
        thread: ThreadId,
        limits: BatchLimits,
        step: impl FnMut(CaptureStep) -> CoreResult<()>,
    ) -> CoreResult<SnapshotInfo> {
        let _capturing = Capturing::start(id);
        let mut written = Written::default();
        let published = self
            .write_manifest(id, limits, &mut written, step)
            .and_then(|()| self.publish(id, thread, written));
        match published {
            Ok(()) => Ok(SnapshotInfo {
                id: id.to_owned(),
                file_count: written.files,
            }),
            Err(error) => {
                self.discard(id, written);
                Err(error)
            }
        }
    }

    /// Read every managed entry and write its manifest row under `id`, with
    /// the blob of any content not stored yet. Repository size is not a
    /// reason to reject an otherwise valid operation; it only means more
    /// batches. `written` counts what it wrote, also when it fails part
    /// way; `step` runs between batches (tests act there).
    fn write_manifest(
        &self,
        id: &str,
        limits: BatchLimits,
        written: &mut Written,
        mut step: impl FnMut(CaptureStep) -> CoreResult<()>,
    ) -> CoreResult<()> {
        let sources = self.source_paths()?;
        // The capture's first row: the rows of a capture are only ever
        // removed together, so while it stands, all of them do.
        let mut first = None;
        let mut batch: Vec<ScannedEntry> = Vec::new();
        let mut bytes = 0_usize;
        for source in &sources {
            let Some(scanned) = self.capture_entry(source, true) else {
                continue;
            };
            let size = scanned.data.as_ref().map_or(0, Vec::len);
            // A batch holds one entry at least, whatever its size.
            if !batch.is_empty()
                && (batch.len() >= limits.entries || bytes.saturating_add(size) > limits.bytes)
            {
                self.write_batch(id, &batch, first.as_deref(), written, &mut step)?;
                first.get_or_insert_with(|| batch[0].manifest.path.clone());
                batch.clear();
                bytes = 0;
            }
            bytes += size;
            batch.push(scanned);
        }
        if !batch.is_empty() {
            self.write_batch(id, &batch, first.as_deref(), written, &mut step)?;
        }
        Ok(())
    }

    /// `first` is the path of the capture's first row, once a batch was
    /// written.
    fn write_batch(
        &self,
        id: &str,
        batch: &[ScannedEntry],
        first: Option<&str>,
        written: &mut Written,
        step: &mut impl FnMut(CaptureStep) -> CoreResult<()>,
    ) -> CoreResult<()> {
        // Content already stored is not compressed again, so a capture of an
        // unchanged tree adds manifest rows only.
        let mut stored = BTreeSet::new();
        {
            let connection = self.connection.lock();
            let mut exists = connection.prepare_cached(BLOB_STORED).map_err(storage)?;
            for entry in batch {
                let hash = &entry.manifest.hash;
                if entry.data.is_some()
                    && !stored.contains(hash.as_str())
                    && exists
                        .query_row([hash], |row| row.get::<_, bool>(0))
                        .map_err(storage)?
                {
                    stored.insert(hash.as_str());
                }
            }
        }
        let mut blobs = BTreeMap::new();
        for entry in batch {
            let hash = entry.manifest.hash.as_str();
            if let Some(data) = &entry.data
                && !stored.contains(hash)
                && !blobs.contains_key(hash)
            {
                blobs.insert(hash, compress(data)?);
                written.compressed += 1;
            }
        }
        step(CaptureStep::Checked)?;

        let mut connection = self.connection.lock();
        // IMMEDIATE, not the default DEFERRED: a deferred read-to-write
        // upgrade racing the other connection returns SQLITE_BUSY without
        // consulting the busy timeout.
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        // Another process's retention pass took the capture for abandoned
        // and removed its rows: it stops now rather than at its last write.
        if let Some(first) = first
            && !tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM snapshot_files WHERE snapshot_id=?1 AND path=?2)",
                    params![id, first],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(storage)?
        {
            return Err(removed_while_written());
        }
        let mut added = 0_usize;
        {
            let mut exists = tx.prepare_cached(BLOB_STORED).map_err(storage)?;
            let mut insert_blob = tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO snapshot_blobs(hash,size,data) VALUES(?1,?2,?3)",
                )
                .map_err(storage)?;
            let mut insert_file = tx
                .prepare_cached(
                    "INSERT INTO snapshot_files(snapshot_id,path,hash,kind,mode,size) \
                     VALUES(?1,?2,?3,?4,?5,?6)",
                )
                .map_err(storage)?;
            for entry in batch {
                let manifest = &entry.manifest;
                if let Some(data) = &entry.data {
                    let size = i64::try_from(data.len()).unwrap_or(i64::MAX);
                    if let Some(compressed) = blobs.get(manifest.hash.as_str()) {
                        added += insert_blob
                            .execute(params![manifest.hash, size, compressed])
                            .map_err(storage)?;
                    } else if !exists
                        .query_row([&manifest.hash], |row| row.get::<_, bool>(0))
                        .map_err(storage)?
                    {
                        // Stored when the batch was checked, but a retention
                        // pass (in any process) removed it since. Checked
                        // again under the write lock, so no manifest row
                        // names a blob that is gone; once this transaction
                        // commits, the row keeps its blob.
                        added += insert_blob
                            .execute(params![manifest.hash, size, compress(data)?])
                            .map_err(storage)?;
                        written.compressed += 1;
                    }
                }
                insert_file
                    .execute(params![
                        id,
                        manifest.path,
                        manifest.hash,
                        manifest.kind.database_name(),
                        manifest.mode.map(i64::from),
                        i64::try_from(manifest.size).unwrap_or(i64::MAX),
                    ])
                    .map_err(storage)?;
            }
        }
        tx.commit().map_err(storage)?;
        drop(connection);
        written.files += u64::try_from(batch.len()).unwrap_or(u64::MAX);
        written.blobs += u64::try_from(added).unwrap_or(u64::MAX);
        step(CaptureStep::Written)
    }

    /// Make the capture a snapshot by writing its `snapshots` row, the last
    /// thing it writes. A retention pass that took the capture for an
    /// abandoned one removed rows it wrote, and then it fails instead of
    /// becoming a snapshot that lacks files.
    fn publish(&self, id: &str, thread: ThreadId, written: Written) -> CoreResult<()> {
        let mut connection = self.connection.lock();
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        let rows: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM snapshot_files WHERE snapshot_id=?1",
                [id],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if u64::try_from(rows).ok() != Some(written.files) {
            return Err(removed_while_written());
        }
        tx.execute(
            "INSERT INTO snapshots(id,thread_id,created_at,file_count) VALUES(?1,?2,?3,?4)",
            params![
                id,
                thread.to_string(),
                Utc::now().to_rfc3339(),
                i64::try_from(written.files).unwrap_or(i64::MAX)
            ],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)
    }

    /// Remove the manifest rows of a capture that failed, a batch per
    /// transaction, and return how many transactions removed some. Rows
    /// this cannot remove go once the capture is abandoned. The blobs it
    /// added go with the next retention pass, which scans every manifest
    /// row for them: one that added none (the content of an unchanged tree
    /// was all stored) asks for no such scan.
    fn discard(&self, id: &str, written: Written) -> usize {
        let mut transactions = 0;
        loop {
            let removed = self.connection.lock().execute(
                "DELETE FROM snapshot_files WHERE rowid IN \
                 (SELECT rowid FROM snapshot_files WHERE snapshot_id=?1 LIMIT ?2) \
                 AND NOT EXISTS(SELECT 1 FROM snapshots WHERE id=?1)",
                params![id, i64::try_from(BATCH.entries).unwrap_or(i64::MAX)],
            );
            match removed {
                Ok(0) => break,
                Ok(_) => transactions += 1,
                Err(error) => {
                    tracing::debug!(snapshot = id, %error, "could not remove a failed capture's entries");
                    break;
                }
            }
        }
        if transactions > 0
            && written.blobs > 0
            && let Some(database) = database_key(&self.connection.lock())
        {
            ORPHANED.lock().insert(database);
        }
        transactions
    }
}

/// Why a capture fails whose rows a retention pass of another process
/// removed, taking it for abandoned. It may reach the person as the error
/// of the tool call that needed the checkpoint.
fn removed_while_written() -> CoreError {
    CoreError::Storage(
        "could not save the undo checkpoint: another Haarness process sharing this data \
         removed it as unfinished while it was being written; try again"
            .into(),
    )
}

fn compress(data: &[u8]) -> CoreResult<Vec<u8>> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(data).map_err(io_error)?;
    encoder.finish().map_err(io_error)
}

#[async_trait]
impl SnapshotStore for SqliteSnapshotStore {
    async fn capture(&self, thread: ThreadId) -> CoreResult<SnapshotInfo> {
        self.blocking(move |store| store.capture_sync(thread)).await
    }

    /// Legacy snapshots predate paired checkpoints. Preserve their historical
    /// overlay behavior, but never delete files that are absent from the old
    /// manifest.
    async fn restore(&self, snapshot_id: &str) -> CoreResult<Vec<String>> {
        let mut manifest = self.load_manifest(snapshot_id)?;
        // An overlay never deletes; a path the snapshot could not read is
        // left as it is rather than refusing the whole restore.
        manifest.retain(|_, entry| !entry.is_unreadable());
        // Validate every path, ancestor, and blob before the first write.
        for entry in manifest.values() {
            let _ = safe_target(&self.workspace, &entry.path)?;
            ensure_safe_ancestors(&self.workspace, &entry.path)?;
            let _ = self.load_blob(entry)?;
        }

        let mut journal = RestoreJournal::new(&self.workspace);
        let mut touched = Vec::new();
        let mut failure = None;
        for entry in manifest.values() {
            let written = self.live_matches(Some(entry)).and_then(|matches| {
                if matches {
                    return Ok(false);
                }
                journal.write(&self.prepare_entry(entry)?)?;
                Ok(true)
            });
            match written {
                Ok(true) => touched.push(entry.path.clone()),
                Ok(false) => {}
                Err(error) => {
                    failure = Some((entry.path.clone(), error));
                    break;
                }
            }
        }
        journal.finish(failure)?;
        Ok(touched)
    }

    async fn restore_delta(
        &self,
        expected_snapshot_id: &str,
        desired_snapshot_id: &str,
    ) -> CoreResult<Vec<String>> {
        let expected = self.load_manifest(expected_snapshot_id)?;
        let desired = self.load_manifest(desired_snapshot_id)?;
        let paths: BTreeSet<&String> = expected.keys().chain(desired.keys()).collect();
        let changed: Vec<String> = paths
            .into_iter()
            .filter(|path| expected.get(*path) != desired.get(*path))
            .cloned()
            .collect();

        let mut prepared = Vec::with_capacity(changed.len());
        // This is deliberately a separate complete preflight pass. If any
        // path contains a later human edit, no earlier path is touched.
        for path in &changed {
            let target = safe_target(&self.workspace, path)?;
            ensure_safe_ancestors(&self.workspace, path)?;
            // What was there when a snapshot could not read it is unknown:
            // writing or deleting it could destroy work, so refuse.
            if [expected.get(path), desired.get(path)]
                .into_iter()
                .flatten()
                .any(ManifestEntry::is_unreadable)
            {
                return Err(CoreError::Conflict(format!(
                    "workspace path could not be read when a snapshot was taken: {path}"
                )));
            }
            let actual = scan_live_entry(&target, path, &self.workspace)?;
            if !optional_entry_matches(expected.get(path), actual.as_ref()) {
                return Err(CoreError::Conflict(format!(
                    "workspace path changed since snapshot: {path}"
                )));
            }
            let desired = desired.get(path).cloned();
            if let Some(entry) = &desired {
                // Validate now and reload immediately before writing. This
                // keeps restore memory bounded to one blob while retaining a
                // fail-before-mutation validation pass.
                let _ = self.load_blob(entry)?;
            }
            prepared.push(PreparedChange {
                path: path.clone(),
                desired,
            });
        }

        let mut touched = Vec::with_capacity(prepared.len());
        // Delete delta-only paths first, then materialize desired entries. A
        // replacement never removes anything outside this explicit path set.
        // The journal makes the whole set one change: a path that fails part
        // way (permissions, a locked file, a full disk) puts every earlier
        // path back, so a retry meets the workspace its checks expect.
        let mut journal = RestoreJournal::new(&self.workspace);
        let mut failure = None;
        let (removals, writes): (Vec<_>, Vec<_>) =
            prepared.iter().partition(|change| change.desired.is_none());
        for change in removals.into_iter().chain(writes) {
            let result = match &change.desired {
                None => journal.remove(&change.path),
                Some(entry) => self
                    .prepare_entry(entry)
                    .and_then(|desired| journal.write(&desired)),
            };
            if let Err(error) = result {
                failure = Some((change.path.clone(), error));
                break;
            }
            touched.push(change.path.clone());
        }
        journal.finish(failure)?;
        Ok(touched)
    }

    async fn diff_live(&self, snapshot_id: &str) -> CoreResult<Vec<FileDiff>> {
        let snapshot_id = snapshot_id.to_owned();
        self.blocking(move |store| {
            let before = store.load_manifest(&snapshot_id)?;
            let after = store.scan_manifest()?;
            store.diff_manifests(&before, &after, true, false, None)
        })
        .await
    }

    async fn diff_snapshots(
        &self,
        before_snapshot_id: &str,
        after_snapshot_id: &str,
    ) -> CoreResult<Vec<FileDiff>> {
        let (before, after) = (before_snapshot_id.to_owned(), after_snapshot_id.to_owned());
        self.blocking(move |store| {
            let before = store.load_manifest(&before)?;
            let after = store.load_manifest(&after)?;
            store.diff_manifests(&before, &after, false, false, None)
        })
        .await
    }

    /// Manifests only: no blob is read to learn that nothing changed.
    async fn snapshots_differ(
        &self,
        before_snapshot_id: &str,
        after_snapshot_id: &str,
    ) -> CoreResult<bool> {
        let (before, after) = (before_snapshot_id.to_owned(), after_snapshot_id.to_owned());
        self.blocking(
            move |store| Ok(store.load_manifest(&before)? != store.load_manifest(&after)?),
        )
        .await
    }

    async fn diff_review(
        &self,
        before: &str,
        after: Option<&str>,
        summary: bool,
        path: Option<&str>,
    ) -> CoreResult<Vec<FileDiff>> {
        let before = before.to_owned();
        let after = after.map(str::to_owned);
        let path = path.map(str::to_owned);
        self.blocking(move |store| {
            let before = store.load_manifest(&before)?;
            let live = after.is_none();
            let after = match after {
                Some(id) => store.load_manifest(&id)?,
                None => store.scan_manifest()?,
            };
            store.diff_manifests(&before, &after, live, summary, path.as_deref())
        })
        .await
    }
}

fn encode_relative_path(path: &Path) -> CoreResult<String> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_str().ok_or_else(|| {
                    CoreError::Invalid(format!(
                        "snapshot path is not valid UTF-8: {}",
                        path.display()
                    ))
                })?;
                if value == ".git" {
                    return Err(CoreError::Invalid(
                        "snapshot path may not contain .git".into(),
                    ));
                }
                components.push(value);
            }
            _ => {
                return Err(CoreError::Invalid(format!(
                    "snapshot path is not a safe relative path: {}",
                    path.display()
                )));
            }
        }
    }
    if components.is_empty() {
        return Err(CoreError::Invalid("snapshot path may not be empty".into()));
    }
    Ok(components.join("/"))
}

fn matches_configured_home(
    workspace: &Path,
    configured_homes: impl IntoIterator<Item = OsString>,
) -> bool {
    configured_homes.into_iter().any(|home| {
        PathBuf::from(home)
            .canonicalize()
            .is_ok_and(|canonical| canonical == workspace)
    })
}

fn decode_relative_path(path: &str) -> CoreResult<PathBuf> {
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        return Err(CoreError::Invalid(format!("unsafe snapshot path {path:?}")));
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(CoreError::Invalid(format!(
            "absolute snapshot path {path:?}"
        )));
    }
    let mut decoded = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(value) if value != OsStr::new(".git") => decoded.push(value),
            _ => {
                return Err(CoreError::Invalid(format!("unsafe snapshot path {path:?}")));
            }
        }
    }
    if decoded.as_os_str().is_empty() {
        return Err(CoreError::Invalid(format!("unsafe snapshot path {path:?}")));
    }
    Ok(decoded)
}

fn safe_target(workspace: &Path, relative: &str) -> CoreResult<PathBuf> {
    Ok(workspace.join(decode_relative_path(relative)?))
}

fn ensure_safe_ancestors(workspace: &Path, relative: &str) -> CoreResult<()> {
    let decoded = decode_relative_path(relative)?;
    let Some(parent) = decoded.parent() else {
        return Ok(());
    };
    let mut current = workspace.to_path_buf();
    for component in parent.components() {
        let Component::Normal(value) = component else {
            return Err(CoreError::Invalid(format!(
                "unsafe snapshot path {relative:?}"
            )));
        };
        current.push(value);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CoreError::Conflict(format!(
                    "snapshot path has symlink ancestor: {relative}"
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(CoreError::Conflict(format!(
                    "snapshot path has non-directory ancestor: {relative}"
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(())
}

/// Create the missing parents of `relative`, recording each directory it
/// creates in `created` so a failed restore can remove them again.
fn create_safe_parents(
    workspace: &Path,
    relative: &str,
    created: &mut Vec<PathBuf>,
) -> CoreResult<()> {
    let decoded = decode_relative_path(relative)?;
    let Some(parent) = decoded.parent() else {
        return Ok(());
    };
    let mut current = workspace.to_path_buf();
    for component in parent.components() {
        let Component::Normal(value) = component else {
            return Err(CoreError::Invalid(format!(
                "unsafe snapshot path {relative:?}"
            )));
        };
        current.push(value);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CoreError::Conflict(format!(
                    "snapshot path has symlink ancestor: {relative}"
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(CoreError::Conflict(format!(
                    "snapshot path has non-directory ancestor: {relative}"
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(io_error)?;
                created.push(current.clone());
                let metadata = fs::symlink_metadata(&current).map_err(io_error)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(CoreError::Conflict(format!(
                        "snapshot parent changed while restoring: {relative}"
                    )));
                }
            }
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(())
}

fn scan_live_entry(
    target: &Path,
    relative: &str,
    workspace: &Path,
) -> CoreResult<Option<ManifestEntry>> {
    let metadata = match fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    if metadata.file_type().is_symlink() {
        let target_value = fs::read_link(target).map_err(io_error)?;
        let data = os_string_bytes(target_value.into_os_string());
        let points_to_directory = target
            .canonicalize()
            .ok()
            .is_some_and(|canonical| canonical.starts_with(workspace) && canonical.is_dir());
        return Ok(Some(ManifestEntry {
            path: relative.to_string(),
            hash: hex_digest(&data),
            kind: if points_to_directory {
                EntryKind::SymlinkDirectory
            } else {
                EntryKind::SymlinkFile
            },
            mode: None,
            size: u64::try_from(data.len()).unwrap_or(u64::MAX),
        }));
    }
    if !metadata.is_file() {
        // Directories and special files deliberately do not compare equal to
        // an absent/file manifest entry.
        return Ok(Some(ManifestEntry {
            path: relative.to_string(),
            hash: String::new(),
            kind: EntryKind::File,
            mode: None,
            size: u64::MAX,
        }));
    }
    let (hash, size) = hash_file(target)?;
    Ok(Some(ManifestEntry {
        path: relative.to_string(),
        hash,
        kind: if size > MAX_FILE_BYTES {
            EntryKind::Omitted
        } else {
            EntryKind::File
        },
        mode: file_mode(&metadata),
        size,
    }))
}

fn entries_match_live(expected: &ManifestEntry, actual: &ManifestEntry) -> bool {
    let kind_matches = expected.kind == actual.kind
        || matches!(
            (expected.kind, actual.kind),
            (EntryKind::File, EntryKind::Omitted) | (EntryKind::Omitted, EntryKind::File)
        );
    kind_matches
        && expected.hash == actual.hash
        && expected.mode == actual.mode
        && expected.size == actual.size
}

fn optional_entry_matches(
    expected: Option<&ManifestEntry>,
    actual: Option<&ManifestEntry>,
) -> bool {
    match (expected, actual) {
        (None, None) => true,
        (Some(expected), Some(actual)) => entries_match_live(expected, actual),
        _ => false,
    }
}

fn read_live_data(target: &Path, entry: &ManifestEntry) -> CoreResult<Option<Vec<u8>>> {
    if entry.kind == EntryKind::Omitted {
        return Ok(None);
    }
    if entry.kind.is_symlink() {
        let metadata = fs::symlink_metadata(target).map_err(io_error)?;
        if !metadata.file_type().is_symlink() {
            return Err(CoreError::Conflict(format!(
                "workspace path changed while producing diff: {}",
                entry.path
            )));
        }
        let data = os_string_bytes(fs::read_link(target).map_err(io_error)?.into_os_string());
        if hex_digest(&data) != entry.hash {
            return Err(CoreError::Conflict(format!(
                "workspace path changed while producing diff: {}",
                entry.path
            )));
        }
        return Ok(Some(data));
    }
    let metadata = fs::symlink_metadata(target).map_err(io_error)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(CoreError::Conflict(format!(
            "workspace path changed while producing diff: {}",
            entry.path
        )));
    }
    let data = fs::read(target).map_err(io_error)?;
    if u64::try_from(data.len()).ok() != Some(entry.size) || hex_digest(&data) != entry.hash {
        return Err(CoreError::Conflict(format!(
            "workspace path changed while producing diff: {}",
            entry.path
        )));
    }
    Ok(Some(data))
}

/// What a restore names its scratch entries: the replacement written beside
/// a path, and the live entry moved aside until the restore is kept.
/// Snapshots never record them, so one a crash leaves behind is neither
/// restored nor reported as a change.
const SCRATCH_NAMES: [(&str, &str); 2] =
    [(".haarness-snapshot-", ".tmp"), (".haarness-undo-", ".bak")];

fn is_restore_scratch(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        SCRATCH_NAMES
            .iter()
            .any(|(prefix, suffix)| name.starts_with(prefix) && name.ends_with(suffix))
    })
}

fn scratch_path(directory: &Path, (prefix, suffix): (&str, &str)) -> PathBuf {
    directory.join(format!("{prefix}{}{suffix}", uuid::Uuid::now_v7()))
}

/// One change a restore made, in the order it made them.
#[derive(Debug)]
enum RestoreStep {
    /// A missing parent directory created for a restored path.
    CreatedDirectory(PathBuf),
    /// The live entry at `target`, renamed to `backup` before its path was
    /// replaced or removed.
    MovedAside { target: PathBuf, backup: PathBuf },
    /// A restored entry, now at this path.
    Wrote(PathBuf),
}

/// Makes a restore of many paths all or nothing. Every entry a restore
/// replaces or removes is first renamed aside in its own directory; a
/// failure undoes the steps newest first, and success deletes what was
/// moved aside.
struct RestoreJournal<'a> {
    workspace: &'a Path,
    steps: Vec<RestoreStep>,
}

impl<'a> RestoreJournal<'a> {
    fn new(workspace: &'a Path) -> Self {
        Self {
            workspace,
            steps: Vec::new(),
        }
    }

    /// Remove whatever is at `relative`.
    fn remove(&mut self, relative: &str) -> CoreResult<()> {
        let target = safe_target(self.workspace, relative)?;
        ensure_safe_ancestors(self.workspace, relative)?;
        self.move_aside(&target, relative)
    }

    /// Put `prepared` at its path, replacing whatever is there.
    fn write(&mut self, prepared: &PreparedEntry) -> CoreResult<()> {
        let relative = &prepared.manifest.path;
        let target = safe_target(self.workspace, relative)?;
        let mut created = Vec::new();
        let parents = create_safe_parents(self.workspace, relative, &mut created);
        self.steps
            .extend(created.into_iter().map(RestoreStep::CreatedDirectory));
        parents?;
        ensure_safe_ancestors(self.workspace, relative)?;
        let parent = target
            .parent()
            .ok_or_else(|| CoreError::Invalid(format!("invalid snapshot path {relative}")))?;
        // Written in full before anything at the path moves, so the path is
        // only ever missing between two renames.
        let temporary = scratch_path(parent, SCRATCH_NAMES[0]);
        let written = write_temporary(&temporary, prepared)
            .and_then(|()| self.move_aside(&target, relative))
            .and_then(|()| fs::rename(&temporary, &target).map_err(io_error));
        if let Err(error) = written {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        self.steps.push(RestoreStep::Wrote(target));
        Ok(())
    }

    fn move_aside(&mut self, target: &Path, relative: &str) -> CoreResult<()> {
        let metadata = match fs::symlink_metadata(target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error(error)),
        };
        // Only an empty directory is ever replaced.
        if metadata.is_dir() && fs::read_dir(target).map_err(io_error)?.next().is_some() {
            return Err(CoreError::Conflict(format!(
                "cannot replace non-empty directory at {relative}"
            )));
        }
        let parent = target
            .parent()
            .ok_or_else(|| CoreError::Invalid(format!("invalid snapshot path {relative}")))?;
        let backup = scratch_path(parent, SCRATCH_NAMES[1]);
        fs::rename(target, &backup).map_err(io_error)?;
        self.steps.push(RestoreStep::MovedAside {
            target: target.to_path_buf(),
            backup,
        });
        Ok(())
    }

    /// Keep every change when nothing failed; otherwise put back everything
    /// already changed and report the path that failed.
    fn finish(self, failure: Option<(String, CoreError)>) -> CoreResult<()> {
        let Some((path, error)) = failure else {
            for step in self.steps {
                if let RestoreStep::MovedAside { backup, .. } = step
                    && let Err(error) = remove_target(&backup)
                {
                    tracing::warn!(
                        path = %backup.display(),
                        %error,
                        "restore could not delete a moved-aside entry"
                    );
                }
            }
            return Ok(());
        };
        let reason = match &error {
            CoreError::Storage(message)
            | CoreError::Conflict(message)
            | CoreError::Invalid(message) => message.clone(),
            other => other.to_string(),
        };
        let workspace = self.workspace;
        let relative = |path: &Path| {
            path.strip_prefix(workspace)
                .unwrap_or(path)
                .display()
                .to_string()
        };
        let mut stuck = Vec::new();
        for step in self.steps.into_iter().rev() {
            match step {
                RestoreStep::Wrote(target) => {
                    if let Err(error) = remove_target(&target) {
                        stuck.push(format!("{} ({error})", relative(&target)));
                    }
                }
                RestoreStep::MovedAside { target, backup } => {
                    if let Err(error) = fs::rename(&backup, &target) {
                        stuck.push(format!(
                            "{} ({error}; its previous content is in {})",
                            relative(&target),
                            relative(&backup)
                        ));
                    }
                }
                RestoreStep::CreatedDirectory(directory) => match fs::remove_dir(&directory) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        stuck.push(format!("{} ({error})", relative(&directory)));
                    }
                    _ => {}
                },
            }
        }
        if stuck.is_empty() {
            return Err(CoreError::Conflict(format!(
                "could not restore {path}: {reason}; no files were changed"
            )));
        }
        tracing::error!(
            %path,
            %reason,
            ?stuck,
            "restore failed and could not put every path back"
        );
        Err(CoreError::Storage(format!(
            "could not restore {path}: {reason}; putting back the files already changed also failed for {}",
            stuck.join(", ")
        )))
    }
}

fn write_temporary(temporary: &Path, prepared: &PreparedEntry) -> CoreResult<()> {
    if prepared.manifest.kind.is_symlink() {
        let link_target = bytes_os_string(&prepared.data)?;
        return create_symlink(
            &link_target,
            temporary,
            prepared.manifest.kind == EntryKind::SymlinkDirectory,
        );
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(temporary)
        .map_err(io_error)?;
    file.write_all(&prepared.data).map_err(io_error)?;
    set_file_mode(&file, prepared.manifest.mode)?;
    file.sync_all().map_err(io_error)
}

fn remove_target(target: &Path) -> CoreResult<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir(target).map_err(io_error)
        }
        Ok(_) => fs::remove_file(target).map_err(io_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

#[cfg(unix)]
fn create_symlink(target: &OsStr, link: &Path, _directory: bool) -> CoreResult<()> {
    std::os::unix::fs::symlink(target, link).map_err(io_error)
}

#[cfg(windows)]
fn create_symlink(target: &OsStr, link: &Path, directory: bool) -> CoreResult<()> {
    if directory {
        std::os::windows::fs::symlink_dir(target, link).map_err(io_error)
    } else {
        std::os::windows::fs::symlink_file(target, link).map_err(io_error)
    }
}

#[cfg(not(any(unix, windows)))]
fn create_symlink(_target: &OsStr, _link: &Path, _directory: bool) -> CoreResult<()> {
    Err(CoreError::Storage(
        "symlink restore is unsupported on this platform".into(),
    ))
}

#[cfg(unix)]
fn os_string_bytes(value: OsString) -> Vec<u8> {
    use std::os::unix::ffi::OsStringExt;
    value.into_vec()
}

#[cfg(windows)]
fn os_string_bytes(value: OsString) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    value
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>()
}

#[cfg(not(any(unix, windows)))]
fn os_string_bytes(value: OsString) -> Vec<u8> {
    value.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn bytes_os_string(value: &[u8]) -> CoreResult<OsString> {
    use std::os::unix::ffi::OsStringExt;
    if value.contains(&0) {
        return Err(CoreError::Storage("symlink target contains NUL".into()));
    }
    Ok(OsString::from_vec(value.to_vec()))
}

#[cfg(windows)]
fn bytes_os_string(value: &[u8]) -> CoreResult<OsString> {
    use std::os::windows::ffi::OsStringExt;
    let chunks = value.chunks_exact(2);
    if !chunks.remainder().is_empty() {
        return Err(CoreError::Storage(
            "symlink target contains invalid UTF-16 bytes".into(),
        ));
    }
    let wide = chunks
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    Ok(OsString::from_wide(&wide))
}

#[cfg(not(any(unix, windows)))]
fn bytes_os_string(value: &[u8]) -> CoreResult<OsString> {
    String::from_utf8(value.to_vec())
        .map(OsString::from)
        .map_err(|_| CoreError::Storage("symlink target is not valid UTF-8".into()))
}

#[cfg(unix)]
// The manifest schema is cross-platform: modes are absent on platforms that
// do not expose Unix permission bits, hence the shared `Option<u32>` shape.
#[allow(clippy::unnecessary_wraps)]
fn file_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.mode() & 0o7777)
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn set_file_mode(file: &File, mode: Option<u32>) -> CoreResult<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode {
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(io_error)?;
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // same signature as on Unix, where setting it can fail
fn set_file_mode(_file: &File, _mode: Option<u32>) -> CoreResult<()> {
    Ok(())
}

fn hash_file(path: &Path) -> CoreResult<(String, u64)> {
    let mut file = File::open(path).map_err(io_error)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut size = 0_u64;
    loop {
        let count = file.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        size = size.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
    }
    Ok((hex_digest_value(&digest.finalize()), size))
}

fn hex_digest(bytes: &[u8]) -> String {
    hex_digest_value(&Sha256::digest(bytes))
}

fn hex_digest_value(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn line_counts(before: &[u8], after: &[u8]) -> (u64, u64) {
    let before = String::from_utf8_lossy(before);
    let after = String::from_utf8_lossy(after);
    let diff = TextDiff::from_lines(&before, &after);
    let mut additions = 0_u64;
    let mut deletions = 0_u64;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }
    (additions, deletions)
}

fn storage(error: impl std::fmt::Display) -> CoreError {
    CoreError::Storage(error.to_string())
}

fn io_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempdir::TempDirGuard, SqliteSnapshotStore) {
        let root = std::env::temp_dir().join(format!("haarness-snap-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("ws/src")).unwrap();
        // Migrations come from the main store opening the same file.
        let database = root.join("test.db");
        crate::SqliteStore::open(&database).unwrap();
        let store = SqliteSnapshotStore::open(&database, root.join("ws")).unwrap();
        (tempdir::TempDirGuard(root), store)
    }

    mod tempdir {
        pub struct TempDirGuard(pub std::path::PathBuf);
        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn count(store: &SqliteSnapshotStore, table: &str) -> i64 {
        store
            .connection
            .lock()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    /// Backdate a snapshot so retention has something expired to find.
    fn age(store: &SqliteSnapshotStore, id: &str, days: i64) {
        let when = (Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        store
            .connection
            .lock()
            .execute(
                "UPDATE snapshots SET created_at=?2 WHERE id=?1",
                params![id, when],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn retention_drops_expired_checkpoints_and_their_orphaned_blobs() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        let thread = ThreadId::new();
        fs::write(workspace.join("a.txt"), "one\n").unwrap();
        let old = store.capture(thread).await.unwrap();
        fs::write(workspace.join("a.txt"), "two\n").unwrap();
        let recent = store.capture(thread).await.unwrap();
        age(&store, &old.id, 30);

        assert_eq!(count(&store, "snapshots"), 2);
        assert_eq!(count(&store, "snapshot_blobs"), 2);
        let report = store.prune(chrono::Duration::days(7), 1).unwrap();
        assert_eq!(report.snapshots, 1);
        assert_eq!(report.blobs, 1, "the blob only the old manifest named");
        assert_eq!(count(&store, "snapshots"), 1);
        assert_eq!(count(&store, "snapshot_files"), 1);
        // The surviving checkpoint still restores.
        fs::write(workspace.join("a.txt"), "three\n").unwrap();
        store.restore(&recent.id).await.unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("a.txt")).unwrap(),
            "two\n"
        );
    }

    #[tokio::test]
    async fn retention_keeps_recent_checkpoints_and_a_floor_per_thread() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        let thread = ThreadId::new();
        let mut ids = Vec::new();
        for index in 0..4 {
            fs::write(workspace.join("a.txt"), format!("v{index}\n")).unwrap();
            let snapshot = store.capture(thread).await.unwrap();
            age(&store, &snapshot.id, 30);
            ids.push(snapshot.id);
        }
        // Everything is expired, but the floor holds the newest two back.
        let report = store.prune(chrono::Duration::days(7), 2).unwrap();
        assert_eq!(report.snapshots, 2);
        assert_eq!(count(&store, "snapshots"), 2);
        assert!(store.restore(ids.last().unwrap()).await.is_ok());

        // Nothing is expired here, so nothing goes.
        let (guard, store) = fixture();
        fs::write(guard.0.join("ws/a.txt"), "fresh\n").unwrap();
        store.capture(ThreadId::new()).await.unwrap();
        assert_eq!(
            store.prune(chrono::Duration::days(7), 0).unwrap(),
            PruneReport::default()
        );
        assert_eq!(count(&store, "snapshots"), 1);
    }

    #[tokio::test]
    async fn retention_never_touches_a_pending_undo() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        let thread = ThreadId::new();
        fs::write(workspace.join("a.txt"), "one\n").unwrap();
        let backup = store.capture(thread).await.unwrap();
        fs::write(workspace.join("a.txt"), "two\n").unwrap();
        let base = store.capture(thread).await.unwrap();
        age(&store, &backup.id, 30);
        age(&store, &base.id, 30);
        // A thread mid-undo holds both sides of the delta.
        store
            .connection
            .lock()
            .execute(
                "INSERT INTO threads(id,title,created_at,updated_at,provider,model,agent,archived,\
                 revert_sequence,revert_snapshot,revert_base_snapshot) \
                 VALUES(?1,'t','now','now','p','m','build',0,1,?2,?3)",
                params![thread.to_string(), backup.id, base.id],
            )
            .unwrap();

        let report = store.prune(chrono::Duration::days(7), 0).unwrap();
        assert_eq!(report.snapshots, 0, "a live undo is not garbage");
        assert_eq!(count(&store, "snapshots"), 2);
    }

    #[tokio::test]
    async fn paired_delta_round_trip_adds_deletes_and_modifies() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("src/main.rs"), "before\n").unwrap();
        fs::write(workspace.join("delete-me"), "present\n").unwrap();
        let before = store.capture(ThreadId::new()).await.unwrap();

        fs::write(workspace.join("src/main.rs"), "after\n").unwrap();
        fs::write(workspace.join("added"), "new\n").unwrap();
        fs::remove_file(workspace.join("delete-me")).unwrap();
        let after = store.capture(ThreadId::new()).await.unwrap();

        let undone = store.restore_delta(&after.id, &before.id).await.unwrap();
        assert_eq!(undone, vec!["added", "delete-me", "src/main.rs"]);
        assert_eq!(
            fs::read_to_string(workspace.join("src/main.rs")).unwrap(),
            "before\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("delete-me")).unwrap(),
            "present\n"
        );
        assert!(!workspace.join("added").exists());

        let redone = store.restore_delta(&before.id, &after.id).await.unwrap();
        assert_eq!(redone, vec!["delete-me", "added", "src/main.rs"]);
        assert_eq!(
            fs::read_to_string(workspace.join("src/main.rs")).unwrap(),
            "after\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("added")).unwrap(),
            "new\n"
        );
        assert!(!workspace.join("delete-me").exists());
    }

    #[tokio::test]
    async fn human_edit_conflict_happens_before_any_write() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("a"), "old-a").unwrap();
        fs::write(workspace.join("b"), "old-b").unwrap();
        let before = store.capture(ThreadId::new()).await.unwrap();
        fs::write(workspace.join("a"), "new-a").unwrap();
        fs::write(workspace.join("b"), "new-b").unwrap();
        let after = store.capture(ThreadId::new()).await.unwrap();
        fs::write(workspace.join("b"), "human-b").unwrap();

        let error = store
            .restore_delta(&after.id, &before.id)
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::Conflict(_)));
        assert_eq!(fs::read_to_string(workspace.join("a")).unwrap(), "new-a");
        assert_eq!(fs::read_to_string(workspace.join("b")).unwrap(), "human-b");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mid_write_failure_rolls_back_every_touched_path() {
        use std::os::unix::fs::PermissionsExt;

        /// Gives the locked directory its permissions back, so cleanup works
        /// even when an assertion fails.
        struct Unlock(PathBuf);
        impl Drop for Unlock {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            }
        }
        fn scratch(directory: &Path) -> Vec<String> {
            let mut found = Vec::new();
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    found.extend(scratch(&entry.path()));
                } else if is_restore_scratch(&entry.file_name()) {
                    found.push(entry.path().display().to_string());
                }
            }
            found
        }
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::create_dir(workspace.join("dir-restored")).unwrap();
        fs::create_dir(workspace.join("locked")).unwrap();
        fs::write(workspace.join("a"), "old-a").unwrap();
        fs::write(workspace.join("dir-restored/d"), "old-d").unwrap();
        fs::write(workspace.join("locked/b"), "old-b").unwrap();
        let before = store.capture(ThreadId::new()).await.unwrap();
        fs::write(workspace.join("a"), "new-a").unwrap();
        fs::remove_dir_all(workspace.join("dir-restored")).unwrap();
        fs::write(workspace.join("added"), "new").unwrap();
        fs::write(workspace.join("locked/b"), "new-b").unwrap();
        let after = store.capture(ThreadId::new()).await.unwrap();

        // Undo removes `added`, then writes `a` and recreates `dir-restored`
        // before it reaches the path it cannot write.
        let unlock = Unlock(workspace.join("locked"));
        fs::set_permissions(&unlock.0, fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(unlock.0.join("probe"), "").is_ok() {
            // Root writes everywhere; there is nothing to fail.
            return;
        }
        for attempt in ["delta", "legacy overlay"] {
            let error = if attempt == "delta" {
                store.restore_delta(&after.id, &before.id).await
            } else {
                store.restore(&before.id).await
            }
            .unwrap_err();
            assert!(
                matches!(error, CoreError::Conflict(_)),
                "{attempt}: {error}"
            );
            let message = error.to_string();
            assert!(
                message.contains("locked/b") && message.contains("no files were changed"),
                "{attempt}: {message}"
            );
            assert_eq!(fs::read_to_string(workspace.join("a")).unwrap(), "new-a");
            assert_eq!(fs::read_to_string(workspace.join("added")).unwrap(), "new");
            assert!(
                !workspace.join("dir-restored").exists(),
                "{attempt}: a directory the restore created goes again"
            );
            assert_eq!(
                fs::read_to_string(workspace.join("locked/b")).unwrap(),
                "new-b"
            );
            assert_eq!(scratch(&workspace), Vec::<String>::new(), "{attempt}");
        }

        // Fixed, the same undo passes its checks and completes.
        drop(unlock);
        let undone = store.restore_delta(&after.id, &before.id).await.unwrap();
        assert_eq!(undone, vec!["added", "a", "dir-restored/d", "locked/b"]);
        assert_eq!(fs::read_to_string(workspace.join("a")).unwrap(), "old-a");
        assert_eq!(
            fs::read_to_string(workspace.join("dir-restored/d")).unwrap(),
            "old-d"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("locked/b")).unwrap(),
            "old-b"
        );
        assert!(!workspace.join("added").exists());
        assert_eq!(scratch(&workspace), Vec::<String>::new());
    }

    /// A chat's folder in the chat home, under a repository whose
    /// `.gitignore` hides everything (a home folder kept in Git): its
    /// checkpoints still hold its files, and its own ignore files still
    /// count. A project's folder reads the ignore files above it, as before.
    #[tokio::test]
    async fn a_chat_home_folder_is_checkpointed_whatever_ignore_files_lie_above_it() {
        let root =
            std::env::temp_dir().join(format!("haarness-snap-home-{}", uuid::Uuid::now_v7()));
        let _guard = tempdir::TempDirGuard(root.clone());
        let chat = root.join("Chat folders/2026-10-28/chat-1a2b");
        fs::create_dir_all(chat.join("src")).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "*\n").unwrap();
        fs::write(chat.join("plan.md"), "plan").unwrap();
        fs::write(chat.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(chat.join("build.log"), "noise").unwrap();
        fs::write(chat.join(".ignore"), "*.log\n").unwrap();
        let database = root.join("test.db");
        crate::SqliteStore::open(&database).unwrap();
        let captured = |folders: SqliteSnapshotFolders| {
            let chat = chat.clone();
            async move {
                haarness_core::SnapshotFolders::open(&folders, &chat).unwrap();
                let store = Arc::clone(folders.inner.stores.lock().get(&chat).unwrap());
                let snapshot = store.capture(ThreadId::new()).await.unwrap();
                store
                    .load_manifest(&snapshot.id)
                    .unwrap()
                    .into_keys()
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(
            captured(SqliteSnapshotFolders::open_in_chat_home(&database)).await,
            [".ignore", "plan.md", "src/main.rs"]
        );
        assert!(
            captured(SqliteSnapshotFolders::open(&database))
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn restore_scratch_entries_are_never_captured() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("kept"), "yes").unwrap();
        fs::write(workspace.join(".haarness-undo-0193.bak"), "left by a crash").unwrap();
        fs::write(workspace.join("src/.haarness-snapshot-0193.tmp"), "half").unwrap();
        fs::write(workspace.join(".haarness-undo-notes"), "a user's file").unwrap();

        let snapshot = store.capture(ThreadId::new()).await.unwrap();
        let manifest = store.load_manifest(&snapshot.id).unwrap();
        assert_eq!(
            manifest.keys().collect::<Vec<_>>(),
            vec![".haarness-undo-notes", "kept"]
        );
    }

    #[tokio::test]
    async fn legacy_restore_is_overlay_and_preserves_extra_files() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("src/main.rs"), "original\n").unwrap();
        let snapshot = store.capture(ThreadId::new()).await.unwrap();
        fs::write(workspace.join("src/main.rs"), "mutated\n").unwrap();
        fs::write(workspace.join("extra"), "human\n").unwrap();

        let touched = store.restore(&snapshot.id).await.unwrap();
        assert_eq!(touched, vec!["src/main.rs"]);
        assert_eq!(
            fs::read_to_string(workspace.join("src/main.rs")).unwrap(),
            "original\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("extra")).unwrap(),
            "human\n"
        );
    }

    #[tokio::test]
    async fn identical_content_shares_blobs_across_snapshots() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("a.txt"), "same-bytes\n").unwrap();
        fs::write(workspace.join("b.txt"), "same-bytes\n").unwrap();
        store.capture(ThreadId::new()).await.unwrap();
        store.capture(ThreadId::new()).await.unwrap();

        let blobs: i64 = store
            .connection
            .lock()
            .query_row("SELECT COUNT(*) FROM snapshot_blobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(blobs, 1, "two files times two snapshots, one stored blob");
    }

    #[tokio::test]
    async fn omitted_large_entry_is_explicit_and_restore_refuses_it() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("huge.bin"), vec![7_u8; 3 * 1024 * 1024]).unwrap();
        let with_large = store.capture(ThreadId::new()).await.unwrap();
        assert_eq!(with_large.file_count, 1);
        fs::remove_file(workspace.join("huge.bin")).unwrap();
        let without_large = store.capture(ThreadId::new()).await.unwrap();

        let error = store
            .restore_delta(&without_large.id, &with_large.id)
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::Conflict(_)));
        assert!(!workspace.join("huge.bin").exists());
    }

    #[tokio::test]
    async fn large_workspace_captures_and_restores_the_complete_manifest() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        for index in 0..10_005 {
            fs::write(workspace.join(format!("entry-{index:05}")), []).unwrap();
        }

        let snapshot = store.capture(ThreadId::new()).await.unwrap();
        assert_eq!(snapshot.file_count, 10_005);
        assert_eq!(store.load_manifest(&snapshot.id).unwrap().len(), 10_005);
        fs::write(workspace.join("entry-10004"), "edited after capture").unwrap();
        store.restore(&snapshot.id).await.unwrap();
        assert_eq!(fs::read(workspace.join("entry-10004")).unwrap(), b"");
    }

    #[tokio::test]
    async fn hidden_files_are_included_but_git_and_ignored_files_are_not() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join(".hidden"), "yes").unwrap();
        fs::write(workspace.join(".gitignore"), "ignored\n").unwrap();
        fs::write(workspace.join("ignored"), "no").unwrap();
        fs::create_dir(workspace.join(".git")).unwrap();
        fs::write(workspace.join(".git/config"), "secret").unwrap();

        let snapshot = store.capture(ThreadId::new()).await.unwrap();
        let manifest = store.load_manifest(&snapshot.id).unwrap();
        assert!(manifest.contains_key(".hidden"));
        assert!(manifest.contains_key(".gitignore"));
        assert!(!manifest.contains_key("ignored"));
        assert!(!manifest.keys().any(|path| path.starts_with(".git/")));
    }

    #[test]
    fn exact_canonical_home_directory_is_detected() {
        let root = std::env::temp_dir().join(format!("haarness-home-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("child")).unwrap();
        let canonical = root.canonicalize().unwrap();
        assert!(matches_configured_home(
            &canonical,
            [root.join("child/..").into_os_string()]
        ));
        assert!(!matches_configured_home(
            &canonical,
            [root.join("child").into_os_string()]
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn snapshot_diff_is_stable_and_live_diff_is_bounded() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("a"), "one\n").unwrap();
        let before = store.capture(ThreadId::new()).await.unwrap();
        fs::write(workspace.join("a"), "two\nthree\n").unwrap();
        let after = store.capture(ThreadId::new()).await.unwrap();

        let stable = store.diff_snapshots(&before.id, &after.id).await.unwrap();
        assert_eq!(stable.len(), 1);
        assert_eq!(stable[0].status, FileDiffStatus::Modified);
        assert_eq!((stable[0].additions, stable[0].deletions), (2, 1));
        assert_eq!(stable[0].before.as_deref(), Some("one\n"));
        assert_eq!(stable[0].after.as_deref(), Some("two\nthree\n"));

        let summary = store
            .diff_review(&before.id, Some(&after.id), true, None)
            .await
            .unwrap();
        assert_eq!((summary[0].additions, summary[0].deletions), (2, 1));
        assert!(summary[0].before.is_none() && summary[0].after.is_none() && !summary[0].truncated);
        assert_eq!(
            store
                .diff_review(&before.id, Some(&after.id), false, Some("a"))
                .await
                .unwrap(),
            stable
        );
        assert!(
            store
                .diff_review(&before.id, Some(&after.id), false, Some("missing"))
                .await
                .unwrap()
                .is_empty()
        );
        fs::write(workspace.join("a"), vec![0_u8; 32]).unwrap();
        let live = store.diff_live(&after.id).await.unwrap();
        assert!(live[0].binary);
        assert!(live[0].before.is_none() && live[0].after.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn delta_preserves_mode_and_symlink_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("script"), "echo before\n").unwrap();
        fs::set_permissions(workspace.join("script"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(workspace.join("target-a"), "a").unwrap();
        fs::write(workspace.join("target-b"), "b").unwrap();
        symlink("target-a", workspace.join("link")).unwrap();
        let before = store.capture(ThreadId::new()).await.unwrap();

        fs::write(workspace.join("script"), "echo after\n").unwrap();
        fs::set_permissions(workspace.join("script"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_file(workspace.join("link")).unwrap();
        symlink("target-b", workspace.join("link")).unwrap();
        let after = store.capture(ThreadId::new()).await.unwrap();

        store.restore_delta(&after.id, &before.id).await.unwrap();
        assert_eq!(
            fs::metadata(workspace.join("script"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_link(workspace.join("link")).unwrap(),
            PathBuf::from("target-a")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ancestor_symlink_swap_is_rejected_without_touching_outside() {
        use std::os::unix::fs::symlink;

        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("src/main.rs"), "before").unwrap();
        let before = store.capture(ThreadId::new()).await.unwrap();
        fs::write(workspace.join("src/main.rs"), "after").unwrap();
        let after = store.capture(ThreadId::new()).await.unwrap();

        let outside = guard.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("main.rs"), "outside").unwrap();
        fs::remove_dir_all(workspace.join("src")).unwrap();
        symlink(&outside, workspace.join("src")).unwrap();

        let error = store
            .restore_delta(&after.id, &before.id)
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::Conflict(_)));
        assert_eq!(
            fs::read_to_string(outside.join("main.rs")).unwrap(),
            "outside"
        );
    }

    #[test]
    fn an_entry_that_changes_while_read_is_read_again_then_recorded_unreadable() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("app.log"), "line\n").unwrap();
        let source = SourcePath {
            absolute: workspace.join("app.log"),
            relative: "app.log".into(),
            unreadable: None,
        };
        let changed = || CoreError::Conflict("snapshot entry changed while capturing".into());

        // Written to during the first read: the second read is kept.
        let mut reads = 0;
        let settled = scan_settled(&source.absolute, || {
            reads += 1;
            if reads == 1 {
                Err(changed())
            } else {
                store.scan_entry(&source, true)
            }
        });
        assert_eq!(reads, 2);
        let Scan::Entry(entry) = settled else {
            panic!("expected the second read, got {settled:?}")
        };
        assert_eq!(entry.manifest.kind, EntryKind::File);
        assert_eq!(entry.data.as_deref(), Some(&b"line\n"[..]));

        // Still changing: recorded as unreadable instead of failing.
        let mut reads = 0;
        let settled = scan_settled(&source.absolute, || {
            reads += 1;
            Err(changed())
        });
        assert_eq!(reads, 2);
        assert!(matches!(settled, Scan::Unreadable(CoreError::Conflict(_))));

        // Deleted after the walk listed it: left out.
        let gone = workspace.join("gone.tmp");
        assert!(matches!(scan_settled(&gone, || Err(changed())), Scan::Gone));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_entries_never_fail_a_capture_and_restores_never_touch_them() {
        use std::os::unix::fs::PermissionsExt;

        /// Gives every locked path its permissions back, so cleanup works
        /// even when an assertion fails.
        struct Unlock(Vec<PathBuf>);
        impl Drop for Unlock {
            fn drop(&mut self) {
                for path in &self.0 {
                    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
                }
            }
        }
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("src/main.rs"), "before\n").unwrap();
        fs::create_dir(workspace.join("docker-data")).unwrap();
        fs::write(workspace.join("docker-data/db"), "root's\n").unwrap();
        fs::write(workspace.join("sealed.log"), "private\n").unwrap();
        let unlock = Unlock(vec![
            workspace.clone(),
            workspace.join("docker-data"),
            workspace.join("sealed.log"),
        ]);
        let mode = |path: &Path, mode| {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        };
        mode(&unlock.0[1], 0o000);
        mode(&unlock.0[2], 0o000);
        if fs::read(&unlock.0[2]).is_ok() {
            // Root reads everything; there is nothing unreadable to test.
            return;
        }

        let before = store.capture(ThreadId::new()).await.unwrap();
        assert_eq!(before.file_count, 3);
        let manifest = store.load_manifest(&before.id).unwrap();
        assert!(manifest["docker-data"].is_unreadable());
        assert!(manifest["sealed.log"].is_unreadable());
        assert_eq!(manifest["src/main.rs"].kind, EntryKind::File);
        fs::write(workspace.join("src/main.rs"), "after\n").unwrap();
        let after = store.capture(ThreadId::new()).await.unwrap();
        assert_eq!(
            *store.warned.lock(),
            BTreeSet::from(["docker-data".to_owned(), "sealed.log".to_owned()]),
            "each path warns once, not once per capture"
        );
        assert_eq!(store.diff_live(&after.id).await.unwrap(), Vec::new());

        // A path that stayed unreadable is no change: undo restores the rest.
        let undone = store.restore_delta(&after.id, &before.id).await.unwrap();
        assert_eq!(undone, vec!["src/main.rs"]);
        assert_eq!(
            fs::read_to_string(workspace.join("src/main.rs")).unwrap(),
            "before\n"
        );

        // Readable now, the directory's file is new to the manifest. Going
        // back to a snapshot that could not see it refuses; nothing is deleted.
        mode(&unlock.0[1], 0o755);
        let readable = store.capture(ThreadId::new()).await.unwrap();
        let error = store
            .restore_delta(&readable.id, &before.id)
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::Conflict(_)), "{error}");
        assert_eq!(
            fs::read_to_string(workspace.join("docker-data/db")).unwrap(),
            "root's\n"
        );

        // The legacy overlay restores what it read and leaves the rest alone.
        fs::write(workspace.join("src/main.rs"), "edited\n").unwrap();
        assert_eq!(
            store.restore(&before.id).await.unwrap(),
            vec!["src/main.rs"]
        );
        assert!(workspace.join("docker-data/db").exists());

        // The workspace root itself must still be readable.
        mode(&workspace, 0o000);
        let error = store.capture(ThreadId::new()).await.unwrap_err();
        assert!(matches!(error, CoreError::Storage(_)), "{error}");
    }

    /// Batches of `entries` entries, whatever their size.
    fn batches_of(entries: usize) -> BatchLimits {
        BatchLimits {
            entries,
            bytes: usize::MAX,
        }
    }

    /// A v7 id as a capture that started `hours` ago has.
    fn started_hours_ago(hours: u64) -> String {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - hours * 60 * 60;
        uuid::Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, seconds, 0)).to_string()
    }

    /// What `write_manifest` wrote, if it completed.
    fn manifest(
        store: &SqliteSnapshotStore,
        id: &str,
        limits: BatchLimits,
        step: impl FnMut(CaptureStep) -> CoreResult<()>,
    ) -> CoreResult<Written> {
        let mut written = Written::default();
        store
            .write_manifest(id, limits, &mut written, step)
            .map(|()| written)
    }

    fn capture_with(store: &SqliteSnapshotStore, limits: BatchLimits) -> (String, Written) {
        let id = uuid::Uuid::now_v7().to_string();
        let written = manifest(store, &id, limits, |_| Ok(())).unwrap();
        store.publish(&id, ThreadId::new(), written).unwrap();
        (id, written)
    }

    /// Whether the next retention pass of `store`'s database scans for the
    /// blobs a failed capture added.
    fn blob_scan_scheduled(store: &SqliteSnapshotStore) -> bool {
        let database = database_key(&store.connection.lock()).unwrap();
        ORPHANED.lock().contains(&database)
    }

    #[test]
    fn a_capture_holds_the_write_lock_for_one_batch_at_most() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        for index in 0..20 {
            fs::write(workspace.join(format!("f{index:02}")), format!("{index}\n")).unwrap();
        }
        // Another chat's writes, on a connection that never waits for a lock.
        let other = Connection::open(guard.0.join("test.db")).unwrap();
        other.busy_timeout(std::time::Duration::ZERO).unwrap();
        let id = uuid::Uuid::now_v7().to_string();
        let mut writes = 0;
        let written = manifest(&store, &id, batches_of(3), |_| {
            other
                .execute_batch(&format!(
                    "BEGIN IMMEDIATE; \
                         INSERT INTO permission_rules(workspace,pattern,decision,created_at) \
                         VALUES('ws','p{writes}','allow','now'); COMMIT;"
                ))
                .map_err(storage)?;
            writes += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(written.files, 20);
        assert_eq!(writes, 14, "seven batches, each checked and written");
        assert!(
            matches!(store.load_manifest(&id), Err(CoreError::NotFound(_))),
            "a capture is no snapshot before its last write"
        );
        store.publish(&id, ThreadId::new(), written).unwrap();
        let manifest = store.load_manifest(&id).unwrap();
        assert_eq!(manifest.len(), 20);
        assert_eq!(store.load_blob(&manifest["f07"]).unwrap(), b"7\n");
    }

    #[tokio::test]
    async fn a_capture_compresses_only_content_no_blob_holds() {
        // In one batch or across batches, equal content is compressed once.
        for limits in [BATCH, batches_of(1)] {
            let (guard, store) = fixture();
            let workspace = guard.0.join("ws");
            fs::write(workspace.join("a"), "same\n").unwrap();
            fs::write(workspace.join("b"), "same\n").unwrap();
            fs::write(workspace.join("c"), "other\n").unwrap();
            fs::write(workspace.join("huge.bin"), vec![7_u8; 3 * 1024 * 1024]).unwrap();
            let (_, first) = capture_with(&store, limits);
            assert_eq!(
                first,
                Written {
                    files: 4,
                    compressed: 2,
                    blobs: 2
                },
                "{limits:?}"
            );
            let (unchanged, again) = capture_with(&store, limits);
            assert_eq!(
                again,
                Written {
                    files: 4,
                    compressed: 0,
                    blobs: 0
                },
                "an unchanged tree compresses nothing: {limits:?}"
            );
            fs::write(workspace.join("c"), "edited\n").unwrap();
            let (edited_id, edited) = capture_with(&store, limits);
            assert_eq!(edited.compressed, 1, "only the edited file: {limits:?}");
            assert_eq!(count(&store, "snapshot_blobs"), 3);

            // The recapture restores like any other.
            assert_eq!(
                store.restore_delta(&edited_id, &unchanged).await.unwrap(),
                vec!["c"]
            );
            assert_eq!(fs::read_to_string(workspace.join("c")).unwrap(), "other\n");
        }
    }

    #[tokio::test]
    async fn retention_during_a_capture_keeps_the_blobs_it_names() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("a"), "one\n").unwrap();
        fs::write(workspace.join("b"), "two\n").unwrap();
        let thread = ThreadId::new();
        let old = store.capture(thread).await.unwrap();
        age(&store, &old.id, 30);
        // A retention pass of another process sharing the database.
        let elsewhere =
            SqliteSnapshotStore::open(guard.0.join("test.db"), workspace.clone()).unwrap();

        let id = uuid::Uuid::now_v7().to_string();
        let mut checked = 0;
        let mut pruned = None;
        let written = manifest(&store, &id, batches_of(1), |step| {
            checked += usize::from(step == CaptureStep::Checked);
            // `a`'s row is written. `b`'s content was found stored, and
            // the pass removes it before `b`'s row is written.
            if step == CaptureStep::Checked && checked == 2 {
                pruned = Some(elsewhere.prune(chrono::Duration::days(7), 0)?);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            pruned.unwrap(),
            PruneReport {
                snapshots: 1,
                blobs: 1,
                abandoned: 0
            },
            "the expired snapshot goes, and of its blobs only `b`'s: the capture's row names `a`'s"
        );
        assert_eq!(
            written,
            Written {
                files: 2,
                compressed: 1,
                blobs: 1
            },
            "`b`'s content is stored again"
        );
        store.publish(&id, thread, written).unwrap();

        fs::write(workspace.join("a"), "changed\n").unwrap();
        fs::remove_file(workspace.join("b")).unwrap();
        assert_eq!(store.restore(&id).await.unwrap(), vec!["a", "b"]);
        assert_eq!(fs::read_to_string(workspace.join("a")).unwrap(), "one\n");
        assert_eq!(fs::read_to_string(workspace.join("b")).unwrap(), "two\n");
    }

    #[test]
    fn a_capture_that_never_completes_is_invisible_and_retention_removes_it() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        for name in ["a", "b", "c"] {
            fs::write(workspace.join(name), format!("{name}\n")).unwrap();
        }
        // Stops after its first batch, as a crash would: nothing removes
        // what it wrote.
        let stopped = |id: &str| {
            manifest(&store, id, batches_of(1), |step| match step {
                CaptureStep::Written => Err(CoreError::Storage("crashed".into())),
                CaptureStep::Checked => Ok(()),
            })
            .unwrap_err();
        };
        let crashed = started_hours_ago(25);
        stopped(&crashed);
        // One that started just now looks like this while it runs, in this
        // process or another.
        fs::write(workspace.join("a"), "a, edited\n").unwrap();
        let running = uuid::Uuid::now_v7().to_string();
        stopped(&running);

        assert_eq!(count(&store, "snapshots"), 0);
        for id in [&crashed, &running] {
            assert!(matches!(
                store.load_manifest(id),
                Err(CoreError::NotFound(_))
            ));
        }
        assert_eq!(count(&store, "snapshot_files"), 2);
        assert_eq!(
            store.prune(chrono::Duration::days(7), 20).unwrap(),
            PruneReport {
                snapshots: 0,
                blobs: 1,
                abandoned: 1
            },
            "the crashed capture's row and the blob only it named"
        );
        assert_eq!(
            count(&store, "snapshot_files"),
            1,
            "the running capture's row"
        );
        assert_eq!(count(&store, "snapshot_blobs"), 1);
        assert_eq!(
            store.prune(chrono::Duration::days(7), 20).unwrap(),
            PruneReport::default()
        );

        // A capture whose rows a pass took for abandoned cannot complete.
        let slow = started_hours_ago(25);
        let written = manifest(&store, &slow, BATCH, |_| Ok(())).unwrap();
        assert_eq!(
            store
                .prune(chrono::Duration::days(7), 20)
                .unwrap()
                .abandoned,
            1
        );
        let error = store.publish(&slow, ThreadId::new(), written).unwrap_err();
        assert!(
            error.to_string().contains("removed it as unfinished"),
            "{error}"
        );
        assert_eq!(count(&store, "snapshots"), 0);

        // A completed one is never abandoned, however old its id.
        let done = started_hours_ago(25);
        let written = manifest(&store, &done, BATCH, |_| Ok(())).unwrap();
        store.publish(&done, ThreadId::new(), written).unwrap();
        assert_eq!(
            store.prune(chrono::Duration::days(7), 20).unwrap(),
            PruneReport::default()
        );
        assert_eq!(store.load_manifest(&done).unwrap().len(), 3);
    }

    #[test]
    fn only_old_v7_ids_count_as_started_before_a_cutoff() {
        let now = Utc::now().timestamp_millis();
        assert!(started_before(
            &started_hours_ago(25),
            now - ABANDONED_AFTER_MS
        ));
        assert!(!started_before(
            &started_hours_ago(23),
            now - ABANDONED_AFTER_MS
        ));
        assert!(!started_before(
            &uuid::Uuid::now_v7().to_string(),
            now - ABANDONED_AFTER_MS
        ));
        // A random (v4) id carries no time.
        assert!(!started_before(
            "0f8fad5b-d9cb-469f-a165-70867728950e",
            i64::MAX
        ));
        assert!(!started_before("not an id", i64::MAX));
    }

    #[test]
    fn a_capture_of_this_process_is_never_taken_for_abandoned() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        for name in ["a", "b", "c"] {
            fs::write(workspace.join(name), format!("{name}\n")).unwrap();
        }
        // Its id is over a day old, as after a laptop slept through it; the
        // pass runs on another connection of this process.
        let id = started_hours_ago(25);
        let elsewhere =
            SqliteSnapshotStore::open(guard.0.join("test.db"), workspace.clone()).unwrap();
        let mut swept = Vec::new();
        let info = store
            .capture_as(&id, ThreadId::new(), batches_of(1), |step| {
                if step == CaptureStep::Written {
                    swept.push(elsewhere.sweep()?);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(swept, vec![PruneReport::default(); 3]);
        assert_eq!(info.file_count, 3);
        assert_eq!(store.load_manifest(&id).unwrap().len(), 3);
        assert!(
            !CAPTURING.lock().contains(&id),
            "done, it is no longer in flight"
        );
    }

    #[test]
    fn a_capture_whose_rows_another_process_removed_stops_at_its_next_batch() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        for name in ["a", "b", "c"] {
            fs::write(workspace.join(name), format!("{name}\n")).unwrap();
        }
        // Another process's capture, a day old: this process's pass may take
        // it for abandoned.
        let id = started_hours_ago(25);
        let mut written = 0;
        let error = manifest(&store, &id, batches_of(1), |step| {
            if step == CaptureStep::Written {
                written += 1;
                assert_eq!(store.sweep()?.abandoned, 1);
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(written, 1, "the next batch stops");
        assert!(error.to_string().contains("try again"), "{error}");
        assert_eq!(count(&store, "snapshot_files"), 0);
    }

    #[test]
    fn retention_keeps_a_capture_that_completes_while_the_pass_runs() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("a"), "a\n").unwrap();
        let id = started_hours_ago(25);
        let written = manifest(&store, &id, BATCH, |_| Ok(())).unwrap();
        let abandoned = abandoned_captures(&store.connection.lock()).unwrap();
        assert_eq!(abandoned, vec![id.clone()]);
        // Another process completes it before the pass removes anything.
        let elsewhere =
            SqliteSnapshotStore::open(guard.0.join("test.db"), workspace.clone()).unwrap();
        elsewhere.publish(&id, ThreadId::new(), written).unwrap();
        let report = reclaim(&store.connection.lock(), &[], &abandoned).unwrap();
        assert_eq!(report.abandoned, 0);
        assert_eq!(report.blobs, 0);
        let manifest = store.load_manifest(&id).unwrap();
        assert_eq!(store.load_blob(&manifest["a"]).unwrap(), b"a\n");
    }

    #[tokio::test]
    async fn a_failed_capture_that_added_no_blob_schedules_no_blob_scan() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("a"), "a\n").unwrap();
        fs::write(workspace.join("b"), "b\n").unwrap();
        store.capture(ThreadId::new()).await.unwrap();
        store
            .connection
            .lock()
            .execute_batch(
                "CREATE TRIGGER refuse BEFORE INSERT ON snapshots \
                 BEGIN SELECT RAISE(ABORT, 'no room'); END;",
            )
            .unwrap();
        // Unchanged: every blob it names was stored already.
        store.capture(ThreadId::new()).await.unwrap_err();
        assert_eq!(count(&store, "snapshot_files"), 2);
        assert!(!blob_scan_scheduled(&store));

        // A new file's content is a blob only the failed capture named.
        fs::write(workspace.join("c"), "c\n").unwrap();
        store.capture(ThreadId::new()).await.unwrap_err();
        assert!(blob_scan_scheduled(&store));
        assert_eq!(store.sweep().unwrap().blobs, 1);
        assert!(!blob_scan_scheduled(&store));
    }

    #[test]
    fn a_failed_capture_is_removed_a_batch_per_transaction() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        for index in 0..2_100 {
            fs::write(workspace.join(format!("f{index:04}")), format!("{index}\n")).unwrap();
        }
        let id = uuid::Uuid::now_v7().to_string();
        let written = manifest(&store, &id, BATCH, |_| Ok(())).unwrap();
        assert_eq!(written.blobs, 2_100);
        assert_eq!(store.discard(&id, written), 3, "1,024, 1,024 and 52 rows");
        assert_eq!(count(&store, "snapshot_files"), 0);
        assert!(blob_scan_scheduled(&store));
    }

    #[tokio::test]
    async fn a_failed_capture_removes_its_rows_and_the_next_pass_its_blobs() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        fs::write(workspace.join("kept"), "kept\n").unwrap();
        let kept = store.capture(ThreadId::new()).await.unwrap();
        age(&store, &kept.id, 30);
        // More rows than one removal takes.
        for index in 0..1_100 {
            fs::write(workspace.join(format!("f{index:04}")), format!("{index}\n")).unwrap();
        }
        store
            .connection
            .lock()
            .execute_batch(
                "CREATE TRIGGER refuse BEFORE INSERT ON snapshots \
                 BEGIN SELECT RAISE(ABORT, 'no room'); END;",
            )
            .unwrap();
        let error = store.capture(ThreadId::new()).await.unwrap_err();
        assert!(error.to_string().contains("no room"), "{error}");
        assert_eq!(
            count(&store, "snapshot_files"),
            1,
            "only the kept checkpoint's"
        );
        assert_eq!(count(&store, "snapshot_blobs"), 1_101);

        // Keeping every checkpoint, however old, the pass removes the blobs
        // only the failed capture stored.
        assert_eq!(
            store.sweep().unwrap(),
            PruneReport {
                snapshots: 0,
                blobs: 1_100,
                abandoned: 0
            }
        );
        assert_eq!(store.sweep().unwrap(), PruneReport::default());
        assert_eq!(store.load_manifest(&kept.id).unwrap().len(), 1);
        store
            .connection
            .lock()
            .execute_batch("DROP TRIGGER refuse")
            .unwrap();
        assert_eq!(
            store.capture(ThreadId::new()).await.unwrap().file_count,
            1_101
        );
    }

    #[test]
    fn a_batch_holds_no_more_content_than_its_limit() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        for name in ["a", "b", "c", "d", "e"] {
            fs::write(workspace.join(name), "four").unwrap();
        }
        // Larger than a batch may hold, so it is a batch of its own.
        fs::write(workspace.join("f"), "twelve bytes").unwrap();
        let mut batches = 0;
        let limits = BatchLimits {
            entries: 100,
            bytes: 10,
        };
        let written = manifest(&store, &uuid::Uuid::now_v7().to_string(), limits, |step| {
            batches += usize::from(step == CaptureStep::Written);
            Ok(())
        })
        .unwrap();
        assert_eq!(written.files, 6);
        assert_eq!(batches, 4, "a b, c d, e, f");
    }

    /// A diff reads the whole tree: the executor that awaits it keeps
    /// running other tasks meanwhile.
    #[tokio::test(flavor = "current_thread")]
    async fn diffs_run_off_the_async_executor() {
        let (guard, store) = fixture();
        let workspace = guard.0.join("ws");
        for index in 0..2_000 {
            fs::write(workspace.join(format!("f{index:04}")), format!("{index}\n")).unwrap();
        }
        let snapshot = store.capture(ThreadId::new()).await.unwrap();
        fs::write(workspace.join("f0001"), "edited\n").unwrap();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ticker = tokio::spawn({
            let ran = Arc::clone(&ran);
            async move {
                loop {
                    ran.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tokio::task::yield_now().await;
                }
            }
        });
        for review in [false, true] {
            let before = ran.load(std::sync::atomic::Ordering::Relaxed);
            let files = if review {
                store.diff_review(&snapshot.id, None, true, None).await
            } else {
                store.diff_live(&snapshot.id).await
            }
            .unwrap();
            assert_eq!(files.len(), 1);
            assert!(
                ran.load(std::sync::atomic::Ordering::Relaxed) > before,
                "review: {review}"
            );
        }
        ticker.abort();
    }
}
