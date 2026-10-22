//! Bounded Git diff support for workspace review.
//!
//! A review only reads, but Git runs programs a repository's own
//! configuration names. Every review read turns off the ones a person's own
//! `git diff` would not run either: the file system monitor, hooks, an
//! external diff or text conversion and signature checks (`run_git`). For a
//! project whose configuration is not trusted, a folder someone else may
//! have prepared, the reads also turn off what `git diff` would run: the
//! repository's own filters ([`filters`]), a second Git in each changed
//! submodule (with that submodule's own configuration), and in a partial
//! clone a fetch of missing objects through the remote's commands
//! ([`Guard`]). A trusted project's review reads as the person's own
//! `git diff` does.

mod filters;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fmt::Write as _,
    io,
    path::{Component, Path},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use haarness_protocol::{DiffTruncation, FileDiff, FileDiffStatus, ReviewFile};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

// Review bounds. A `/diff` response is read by a person in one sitting, so
// each bound sits where the next byte stops being review material and starts
// being a denial of service on the client that has to decode and paint it.
//
/// Maximum number of changed files one review describes.
///
/// A thousand files is already several times the largest change anyone reads
/// as a unit; past it a client is paging a migration, not reviewing. Applying
/// the bound *before* any per-file work is also what keeps the request short:
/// the work that used to dominate this endpoint was per-file, so an unbounded
/// file count was an unbounded response time.
pub const MAX_FILES: usize = 1000;
/// Maximum patch body retained for one file.
///
/// 512 KiB is roughly twelve thousand lines of one file — far past what a
/// reviewer reads — and it stops a single generated lockfile or vendored blob
/// from spending the whole response on itself.
pub const MAX_PATCH_BYTES: usize = 512 * 1024;
/// Maximum combined patch body retained for a response.
///
/// 8 MiB of patch text is on the order of two hundred thousand lines. It is
/// the bound that actually binds on a large working tree, and it keeps the
/// JSON a terminal or Electron client must parse inside one short request.
pub const MAX_TOTAL_PATCH_BYTES: usize = 8 * 1024 * 1024;

/// Bytes inspected before calling a file binary. This is Git's own heuristic:
/// a NUL in the first 8000 bytes means "not text".
const BINARY_SNIFF_BYTES: usize = 8000;
/// Untracked files are read straight from disk, so the useful concurrency is
/// the filesystem's, not the CPU's. Sixteen keeps the peak retained body at
/// `16 * MAX_PATCH_BYTES` while saturating an SSD.
const UNTRACKED_CONCURRENCY: usize = 16;
const READ_CHUNK_BYTES: usize = 64 * 1024;

const DEFAULT_CONTEXT_LINES: u32 = i32::MAX as u32;
const MAX_METADATA_BYTES: usize = 10_000_000;
const MAX_STDERR_BYTES: usize = 64 * 1024;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const PROCESS_KILL_GRACE: Duration = Duration::from_secs(1);

/// Source used to calculate a Git review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    /// Changes in the index and worktree relative to `HEAD`.
    WorkingTree,
    /// Changes relative to the merge base of `HEAD` and the default branch.
    Branch,
}

/// A detected default branch and the Git ref that resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultBranch {
    pub name: String,
    pub reference: String,
}

#[derive(Debug)]
struct BoundedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug)]
struct GitOutput {
    status: ExitStatus,
    stdout: BoundedBytes,
    stderr: BoundedBytes,
}

impl GitOutput {
    fn success(&self) -> bool {
        self.status.success()
    }

    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout.bytes).into_owned()
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr.bytes)
            .trim()
            .to_owned()
    }
}

#[derive(Debug)]
struct DiffEntry {
    status: FileDiffStatus,
    additions: u64,
    deletions: u64,
    patch: Option<String>,
    binary: bool,
    truncated: bool,
    truncation: Option<DiffTruncation>,
}

impl Default for DiffEntry {
    fn default() -> Self {
        Self {
            status: FileDiffStatus::Modified,
            additions: 0,
            deletions: 0,
            patch: None,
            binary: false,
            truncated: false,
            truncation: None,
        }
    }
}

/// The review of `workspace` with `context_lines` of context: every file's
/// bounded patch, only metadata when `summary` is set, or one literal
/// workspace `path`.
pub async fn diff_selected(
    workspace: &Path,
    mode: DiffMode,
    context_lines: u32,
    summary: bool,
    path: Option<&str>,
    trusted: bool,
) -> Result<Vec<ReviewFile>> {
    if let Some(path) = path {
        if !safe_relative_path(path) {
            bail!("diff path must be a relative workspace file");
        }
    }
    ensure_worktree(workspace).await?;
    let guard = if trusted {
        Guard::default()
    } else {
        Guard::untrusted(workspace).await?
    };

    let Some(reference) = diff_reference(workspace, mode).await? else {
        return Ok(Vec::new());
    };
    let context_lines = context_lines.min(DEFAULT_CONTEXT_LINES);
    // One explicitly named file is the viewer's deferred-detail fetch: the
    // reader opened that file, so it may spend the whole response budget on
    // itself instead of the review-wide per-file share.
    let file_budget = if path.is_some() {
        MAX_TOTAL_PATCH_BYTES
    } else {
        MAX_PATCH_BYTES
    };

    let patch_args = selected_args(
        tracked_diff_args(&reference, context_lines, DiffOutput::Patch, &guard),
        path,
    );
    let numstat_args = selected_args(
        tracked_diff_args(&reference, context_lines, DiffOutput::Numstat, &guard),
        path,
    );
    let status_args = selected_args(
        tracked_diff_args(&reference, context_lines, DiffOutput::Status, &guard),
        path,
    );
    let untracked_args = os_args(&[
        "-c",
        "core.quotepath=false",
        "ls-files",
        "-z",
        "--others",
        "--exclude-standard",
        "--",
        ".",
    ]);

    let untracked_args = selected_args(untracked_args, path);
    let (patch_output, numstat_output, status_output, untracked_output) = tokio::try_join!(
        async {
            if summary {
                Ok(None)
            } else {
                run_git_with(workspace, &patch_args, MAX_TOTAL_PATCH_BYTES, &guard)
                    .await
                    .map(Some)
            }
        },
        run_git_with(workspace, &numstat_args, MAX_METADATA_BYTES, &guard),
        run_git_with(workspace, &status_args, MAX_METADATA_BYTES, &guard),
        run_git(workspace, &untracked_args, MAX_METADATA_BYTES),
    )?;

    // A partial clone whose files the review may not download says what to
    // do instead of quoting Git.
    let explained = |result: Result<()>| {
        result.map_err(|error| {
            let text = format!("{error:#}");
            if !trusted
                && (text.contains("lazy fetching disabled")
                    || text.contains("from promisor remote"))
            {
                anyhow!("{MISSING_OBJECTS}")
            } else {
                error
            }
        })
    };
    if let Some(output) = &patch_output {
        explained(require_regular_diff(output, "read Git patch"))?;
    }
    explained(require_metadata(&numstat_output, "read Git numstat"))?;
    explained(require_metadata(&status_output, "read Git file statuses"))?;
    require_metadata(&untracked_output, "list untracked files")?;

    let mut entries = BTreeMap::<String, DiffEntry>::new();
    for stat in parse_numstat_records(&numstat_output.stdout.bytes) {
        let entry = entries.entry(stat.file).or_default();
        entry.additions = stat.additions;
        entry.deletions = stat.deletions;
        entry.binary = stat.binary;
        if stat.binary {
            entry.truncation = Some(DiffTruncation::Binary);
        }
    }
    for (file, status) in parse_status_records(&status_output.stdout.bytes) {
        entries.entry(file).or_default().status = status;
    }

    let mut untracked = nul_utf8_paths(&untracked_output.stdout.bytes)
        .into_iter()
        .filter(|file| !entries.contains_key(file) && path.is_none_or(|path| path == file))
        .collect::<Vec<_>>();
    // Bound the file set before touching a single file. Tracked changes come
    // first: they are the work under review, and dropping them in favour of
    // whatever untracked scratch a working tree happens to hold would make
    // the review describe the wrong thing.
    let mut omitted_files = 0usize;
    if path.is_none() {
        omitted_files = entries
            .len()
            .saturating_add(untracked.len())
            .saturating_sub(MAX_FILES);
        if entries.len() > MAX_FILES
            && let Some(cut) = entries.keys().nth(MAX_FILES).cloned()
        {
            entries.split_off(&cut);
        }
        untracked.truncate(MAX_FILES.saturating_sub(entries.len()));
    }

    let patch_text = patch_output
        .as_ref()
        .map(GitOutput::stdout_text)
        .unwrap_or_default();
    let truncated = patch_output
        .as_ref()
        .is_some_and(|output| output.stdout.truncated);
    let chunks = split_git_patch(&patch_text, truncated);
    let mut returned_patch_bytes = 0usize;
    for chunk in chunks {
        let Some(file) = file_from_patch_chunk(chunk) else {
            continue;
        };
        let Some(entry) = entries.get_mut(&file) else {
            continue;
        };
        if entry.binary {
            // Git's "body" for a binary change is a one-line notice that the
            // files differ. Report it as binary instead of shipping prose a
            // reviewer cannot read and a diff viewer cannot lay out.
            continue;
        }
        append_patch(entry, chunk, &mut returned_patch_bytes, file_budget);
    }
    if truncated {
        for entry in entries
            .values_mut()
            .filter(|entry| entry.patch.is_none() && !entry.binary)
        {
            entry.truncated = true;
            entry
                .truncation
                .get_or_insert(DiffTruncation::ResponseTooLarge);
        }
    }

    // Untracked files used to cost two Git process startups each, run one
    // after another; on a working tree with a few thousand new files that
    // alone was minutes. Every line of a new file is an addition, so there is
    // nothing for Git to compute: read the file once, concurrently, and build
    // the patch here. `buffered` (not `buffer_unordered`) keeps path order, so
    // which files get bodies out of the shared budget stays deterministic.
    let scan_budget = (!summary).then_some(file_budget);
    let mut scans = futures::stream::iter(untracked.into_iter().map(|file| async move {
        let scan = scan_untracked(workspace, &file, scan_budget).await?;
        Ok::<_, anyhow::Error>((file, scan))
    }))
    .buffered(UNTRACKED_CONCURRENCY);
    while let Some(scanned) = scans.next().await {
        let (file, scan) = scanned?;
        let Some(scan) = scan else {
            continue;
        };
        let mut entry = DiffEntry {
            status: FileDiffStatus::Added,
            additions: scan.lines,
            binary: scan.binary,
            ..DiffEntry::default()
        };
        if scan.binary {
            entry.truncation = Some(DiffTruncation::Binary);
        } else if !summary {
            match untracked_patch(&file, &scan) {
                Some(body) => {
                    append_patch(&mut entry, &body, &mut returned_patch_bytes, file_budget);
                }
                None if scan.over_budget => {
                    entry.truncated = true;
                    entry.truncation = Some(DiffTruncation::FileTooLarge);
                }
                None => {}
            }
        }
        entries.insert(file, entry);
    }

    let omitted_files =
        (omitted_files > 0).then(|| u64::try_from(omitted_files).unwrap_or(u64::MAX));
    Ok(entries
        .into_iter()
        .filter(|(file, _)| path.is_none_or(|path| path == file))
        .map(|(file, entry)| ReviewFile {
            diff: FileDiff {
                file,
                status: entry.status,
                additions: entry.additions,
                deletions: entry.deletions,
                patch: entry.patch,
                before: None,
                after: None,
                binary: entry.binary,
                truncated: entry.truncated,
            },
            truncation: entry.truncation,
            omitted_files,
        })
        .collect())
}

async fn ensure_worktree(workspace: &Path) -> Result<()> {
    if !workspace.is_dir() {
        bail!("workspace is not a directory: {}", workspace.display());
    }
    let output = run_git(
        workspace,
        &os_args(&["rev-parse", "--is-inside-work-tree"]),
        64,
    )
    .await?;
    if !output.success() || output.stdout.truncated || output.stdout_text().trim() != "true" {
        bail!(
            "workspace is not inside a Git worktree: {}",
            workspace.display()
        );
    }
    Ok(())
}

async fn diff_reference(workspace: &Path, mode: DiffMode) -> Result<Option<String>> {
    if mode == DiffMode::WorkingTree {
        if has_head(workspace).await? {
            return Ok(Some("HEAD".to_owned()));
        }
        return Ok(Some(empty_tree(workspace).await?));
    }

    let Some(current) = current_branch(workspace).await? else {
        return Ok(None);
    };
    let Some(base) = detect_default_branch_inner(workspace).await? else {
        return Ok(None);
    };
    if current == base.name {
        return Ok(None);
    }

    let args = vec![
        OsString::from("merge-base"),
        OsString::from(base.reference),
        OsString::from("HEAD"),
    ];
    let output = run_git(workspace, &args, 1024).await?;
    if !output.success() || output.stdout.truncated {
        return Ok(None);
    }
    let reference = output.stdout_text().trim().to_owned();
    Ok((!reference.is_empty()).then_some(reference))
}

async fn has_head(workspace: &Path) -> Result<bool> {
    let output = run_git(
        workspace,
        &os_args(&["rev-parse", "--verify", "HEAD"]),
        1024,
    )
    .await?;
    Ok(output.success() && !output.stdout.truncated)
}

async fn empty_tree(workspace: &Path) -> Result<String> {
    // `hash-object` follows the repository's object format, unlike the usual
    // hard-coded SHA-1 empty-tree ID. Git recognizes this canonical object ID
    // without writing anything into a completely unborn repository.
    let output = run_git(
        workspace,
        &os_args(&["hash-object", "-t", "tree", "--stdin"]),
        1024,
    )
    .await?;
    require_metadata(&output, "create empty Git tree")?;
    let hash = output.stdout_text().trim().to_owned();
    if hash.is_empty() {
        bail!("Git returned an empty object ID for the empty tree");
    }
    Ok(hash)
}

async fn current_branch(workspace: &Path) -> Result<Option<String>> {
    let output = run_git(
        workspace,
        &os_args(&["rev-parse", "--abbrev-ref", "HEAD"]),
        4096,
    )
    .await?;
    if !output.success() || output.stdout.truncated {
        return Ok(None);
    }
    let branch = output.stdout_text().trim().to_owned();
    Ok((!branch.is_empty()).then_some(branch))
}

async fn detect_default_branch_inner(workspace: &Path) -> Result<Option<DefaultBranch>> {
    let remotes = command_lines(workspace, &["remote"]).await?;
    let remote = if remotes.iter().any(|item| item == "origin") {
        Some("origin")
    } else if remotes.len() == 1 {
        remotes.first().map(String::as_str)
    } else if remotes.iter().any(|item| item == "upstream") {
        Some("upstream")
    } else {
        None
    };

    if let Some(remote) = remote {
        let symbolic = format!("refs/remotes/{remote}/HEAD");
        let args = vec![OsString::from("symbolic-ref"), OsString::from(symbolic)];
        let output = run_git(workspace, &args, 4096).await?;
        if output.success() && !output.stdout.truncated {
            let reference = output
                .stdout_text()
                .trim()
                .strip_prefix("refs/remotes/")
                .unwrap_or_default()
                .to_owned();
            let prefix = format!("{remote}/");
            if let Some(name) = reference.strip_prefix(&prefix)
                && !name.is_empty()
            {
                return Ok(Some(DefaultBranch {
                    name: name.to_owned(),
                    reference,
                }));
            }
        }
    }

    let branches = command_lines(
        workspace,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .await?;
    let configured = command_line_optional(workspace, &["config", "init.defaultBranch"]).await?;
    if let Some(name) = configured.filter(|name| branches.contains(name)) {
        return Ok(Some(DefaultBranch {
            reference: name.clone(),
            name,
        }));
    }
    for name in ["main", "master"] {
        if branches.iter().any(|branch| branch == name) {
            return Ok(Some(DefaultBranch {
                name: name.to_owned(),
                reference: name.to_owned(),
            }));
        }
    }
    Ok(None)
}

async fn command_lines(workspace: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = run_git(workspace, &os_args(args), MAX_METADATA_BYTES).await?;
    if !output.success() || output.stdout.truncated {
        return Ok(Vec::new());
    }
    Ok(output
        .stdout_text()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

async fn command_line_optional(workspace: &Path, args: &[&str]) -> Result<Option<String>> {
    Ok(command_lines(workspace, args).await?.into_iter().next())
}

#[derive(Debug, Clone, Copy)]
enum DiffOutput {
    Patch,
    Numstat,
    Status,
}

fn tracked_diff_args(
    reference: &str,
    context_lines: u32,
    output: DiffOutput,
    guard: &Guard,
) -> Vec<OsString> {
    let mut args = os_args(&[
        "-c",
        "core.quotepath=false",
        "diff",
        "--relative",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
    ]);
    // A submodule is not checked for uncommitted changes: that runs a second
    // Git in it, with the submodule's own configuration. One moved to another
    // commit is still listed.
    if guard.untrusted {
        args.push(OsString::from("--ignore-submodules=dirty"));
    }
    match output {
        DiffOutput::Patch => args.push(OsString::from(format!("--unified={context_lines}"))),
        DiffOutput::Numstat => {
            args.push(OsString::from("--numstat"));
            args.push(OsString::from("-z"));
        }
        DiffOutput::Status => {
            args.push(OsString::from("--name-status"));
            args.push(OsString::from("-z"));
        }
    }
    args.push(OsString::from(reference));
    args.push(OsString::from("--"));
    args.push(OsString::from("."));
    args
}

fn selected_args(mut args: Vec<OsString>, path: Option<&str>) -> Vec<OsString> {
    if let Some(path) = path {
        // The final argument is the scope. Literal pathspecs prevent wildcard
        // and magic-pathspec expansion while retaining --relative semantics.
        *args.last_mut().expect("diff scope") = OsString::from(format!(":(literal){path}"));
    }
    args
}

/// One untracked file's review facts, gathered in a single pass that never
/// holds more than the caller's budget in memory.
#[derive(Debug)]
struct FileScan {
    /// Lines the new file adds, counted the way `git diff --numstat` does: a
    /// final line without its newline still counts as a line.
    lines: u64,
    binary: bool,
    /// The whole content, captured only when it fitted the budget.
    body: Option<String>,
    /// Content was read but deliberately not kept, because it did not fit.
    over_budget: bool,
    /// The Git file mode the synthesized header reports.
    mode: &'static str,
}

const REGULAR_MODE: &str = "100644";
#[cfg(unix)]
const EXECUTABLE_MODE: &str = "100755";
const SYMLINK_MODE: &str = "120000";

/// Read one untracked file once: line count, binary verdict, and — when it
/// fits `budget` — its content. `budget` of `None` is a metadata-only scan.
async fn scan_untracked(
    workspace: &Path,
    file: &str,
    budget: Option<usize>,
) -> Result<Option<FileScan>> {
    if !safe_relative_path(file) {
        return Ok(None);
    }
    let path = workspace.join(file);
    // `symlink_metadata`, never `metadata`: following the link would diff the
    // target's bytes, and the target may sit entirely outside the reviewed
    // scope. Git shows the link text, so this does too.
    let Ok(metadata) = tokio::fs::symlink_metadata(&path).await else {
        // Listed a moment ago and gone now: a concurrent delete, not an error.
        return Ok(None);
    };
    if metadata.is_symlink() {
        let Ok(target) = tokio::fs::read_link(&path).await else {
            return Ok(None);
        };
        let text = target.to_string_lossy().into_owned();
        let fits = budget.is_some_and(|budget| text.len() <= budget);
        return Ok(Some(FileScan {
            lines: 1,
            binary: false,
            body: fits.then_some(text),
            over_budget: budget.is_some() && !fits,
            mode: SYMLINK_MODE,
        }));
    }
    if !metadata.is_file() {
        // Directories, sockets and FIFOs are not review material.
        return Ok(None);
    }
    let mode = file_mode(&metadata);

    let Ok(mut handle) = tokio::fs::File::open(&path).await else {
        return Ok(None);
    };
    let mut buffer = vec![0u8; READ_CHUNK_BYTES];
    let mut captured = budget.map(|budget| Vec::with_capacity(budget.min(READ_CHUNK_BYTES)));
    let mut read_total = 0usize;
    let mut lines = 0u64;
    let mut last = b'\n';
    let mut sniffed = 0usize;
    loop {
        let read = handle
            .read(&mut buffer)
            .await
            .with_context(|| format!("failed reading untracked file {file}"))?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        if sniffed < BINARY_SNIFF_BYTES {
            let window = &chunk[..read.min(BINARY_SNIFF_BYTES - sniffed)];
            sniffed += window.len();
            if window.contains(&0) {
                // Binary: stop here rather than stream a blob through just to
                // count lines nobody will read.
                return Ok(Some(FileScan {
                    lines: 0,
                    binary: true,
                    body: None,
                    over_budget: false,
                    mode,
                }));
            }
        }
        lines += chunk
            .iter()
            .fold(0_u64, |count, byte| count + u64::from(*byte == b'\n'));
        last = chunk[read - 1];
        read_total += read;
        // Past the budget the content is dropped, but the read continues: the
        // line counts stay honest even for a file too big to show.
        if captured.is_some() && read_total > budget.unwrap_or(0) {
            captured = None;
        } else if let Some(body) = &mut captured {
            body.extend_from_slice(chunk);
        }
    }
    if read_total > 0 && last != b'\n' {
        lines += 1;
    }
    Ok(Some(FileScan {
        lines,
        binary: false,
        over_budget: captured.is_none() && budget.is_some() && read_total > 0,
        body: captured.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        mode,
    }))
}

#[cfg(unix)]
fn file_mode(metadata: &std::fs::Metadata) -> &'static str {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o111 == 0 {
        REGULAR_MODE
    } else {
        EXECUTABLE_MODE
    }
}

#[cfg(not(unix))]
fn file_mode(_metadata: &std::fs::Metadata) -> &'static str {
    REGULAR_MODE
}

/// Build the unified patch for a brand-new file.
///
/// Every line of an untracked file is an addition, so the requested context
/// width cannot change the result and there is nothing for Git to compute.
/// The `index` header line is deliberately absent: it carries blob IDs no
/// review reads, and producing one would mean hashing content that may have
/// been dropped for being over budget.
fn untracked_patch(file: &str, scan: &FileScan) -> Option<String> {
    let body = scan.body.as_ref()?;
    if scan.lines == 0 {
        return None;
    }
    let name = quote_git_path(file);
    let mut patch = String::with_capacity(body.len() + name.len() * 3 + 128);
    let _ = writeln!(patch, "diff --git a/{name} b/{name}");
    let _ = writeln!(patch, "new file mode {}", scan.mode);
    patch.push_str("--- /dev/null\n");
    let _ = writeln!(patch, "+++ b/{name}");
    if scan.lines == 1 {
        patch.push_str("@@ -0,0 +1 @@\n");
    } else {
        let _ = writeln!(patch, "@@ -0,0 +1,{} @@", scan.lines);
    }
    for line in body.split_inclusive('\n') {
        patch.push('+');
        match line.strip_suffix('\n') {
            Some(text) => {
                patch.push_str(text);
                patch.push('\n');
            }
            None => {
                patch.push_str(line);
                patch.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    Some(patch)
}

/// Git's own header quoting: a path holding a control character, a quote or a
/// backslash is emitted C-quoted so the header stays a single line.
fn quote_git_path(file: &str) -> String {
    let needs_quoting = file
        .chars()
        .any(|value| value == '"' || value == '\\' || value.is_control());
    if !needs_quoting {
        return file.to_owned();
    }
    let mut quoted = String::with_capacity(file.len() + 2);
    quoted.push('"');
    for value in file.chars() {
        match value {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\t' => quoted.push_str("\\t"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            value if value.is_control() => {
                let _ = write!(quoted, "\\{:03o}", value as u32);
            }
            value => quoted.push(value),
        }
    }
    quoted.push('"');
    quoted
}

fn append_patch(
    entry: &mut DiffEntry,
    patch: &str,
    total_patch_bytes: &mut usize,
    file_budget: usize,
) {
    let patch_bytes = patch.len();
    let existing_bytes = entry.patch.as_ref().map_or(0, String::len);
    let over_file = existing_bytes.saturating_add(patch_bytes) > file_budget;
    let over_total = total_patch_bytes.saturating_add(patch_bytes) > MAX_TOTAL_PATCH_BYTES;
    if over_file || over_total {
        if let Some(existing) = entry.patch.take() {
            *total_patch_bytes = total_patch_bytes.saturating_sub(existing.len());
        }
        entry.truncated = true;
        entry.truncation = Some(if over_file {
            DiffTruncation::FileTooLarge
        } else {
            DiffTruncation::ResponseTooLarge
        });
        return;
    }

    entry.patch.get_or_insert_with(String::new).push_str(patch);
    *total_patch_bytes += patch_bytes;
}

fn require_regular_diff(output: &GitOutput, operation: &str) -> Result<()> {
    if output.stderr.truncated {
        bail!("{operation}: Git stderr exceeded {MAX_STDERR_BYTES} bytes");
    }
    if output.success() || output.stdout.truncated {
        return Ok(());
    }
    Err(git_error(operation, output))
}

fn require_metadata(output: &GitOutput, operation: &str) -> Result<()> {
    if output.stdout.truncated {
        bail!("{operation}: Git output exceeded {MAX_METADATA_BYTES} bytes");
    }
    if output.stderr.truncated {
        bail!("{operation}: Git stderr exceeded {MAX_STDERR_BYTES} bytes");
    }
    if output.success() {
        return Ok(());
    }
    Err(git_error(operation, output))
}

/// What the review of an untrusted project says when a partial clone lacks
/// objects it needs: it does not fetch them (`GIT_NO_LAZY_FETCH`), and
/// `git backfill` (Git 2.49 and later) does. The clients recognise "has not
/// downloaded the files the review needs" and show it in their own words.
pub const MISSING_OBJECTS: &str = "this clone has not downloaded the files the review needs, and the review does not download them while project configuration is not trusted; run git backfill (Git 2.49 or later), then try again";

fn git_error(operation: &str, output: &GitOutput) -> anyhow::Error {
    let detail = output.stderr_text();
    if detail.is_empty() {
        anyhow!("{operation}: Git exited with status {}", output.status)
    } else {
        anyhow!(
            "{operation}: Git exited with status {}: {detail}",
            output.status
        )
    }
}

/// What the diffs that read changed files add for a project whose
/// configuration is not trusted. Empty (`Default`) for a trusted one.
#[derive(Debug, Default)]
struct Guard {
    untrusted: bool,
    /// `-c` settings: submodules as a commit line, and the emptied filters
    /// on Git before 2.31.
    args: Vec<OsString>,
    /// `GIT_NO_LAZY_FETCH` (Git 2.45 and later), and the emptied filters as
    /// `GIT_CONFIG_*` on Git 2.31 and later.
    env: Vec<(String, String)>,
}

impl Guard {
    /// Reads the repository's filters (see [`filters`]). When they cannot be
    /// read, or not turned off with this Git, Git is not run.
    async fn untrusted(workspace: &Path) -> Result<Self> {
        let refused = |detail: &str| {
            anyhow!(
                "cannot read the repository's Git settings, so Git was not run{}{detail}",
                if detail.is_empty() { "" } else { ": " }
            )
        };
        let mut scoped = true;
        let mut output = run_git(
            workspace,
            &os_args(&["config", "--show-scope", "-z", "--list"]),
            MAX_METADATA_BYTES,
        )
        .await?;
        // Git before 2.26 has no `--show-scope`: every filter then counts as
        // the repository's own, the user's included.
        if output.status.code() == Some(129) {
            scoped = false;
            output = run_git(
                workspace,
                &os_args(&["config", "--includes", "-z", "--list"]),
                MAX_METADATA_BYTES,
            )
            .await?;
        }
        if !output.success() || output.stdout.truncated {
            let detail = output.stderr_text();
            return Err(refused(detail.lines().next().unwrap_or_default()));
        }
        let pairs = filters::overrides(&output.stdout.bytes, scoped)
            .map_err(|_| refused("a filter's name is not UTF-8"))?;
        let base = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|count| count.trim().parse().ok())
            .unwrap_or(0);
        let version = if pairs.is_empty() {
            None
        } else {
            git_version(workspace).await
        };
        let (filter_args, filter_env) = filters::carry(&pairs, version, base).map_err(|_| {
            anyhow!(
                "the repository names a filter that Git before 2.31 cannot turn off (its name holds =), so Git was not run; update Git, or trust this project's configuration"
            )
        })?;
        let mut args = os_args(&["-c", "diff.submodule=short"]);
        args.extend(filter_args);
        let mut env = vec![("GIT_NO_LAZY_FETCH".to_owned(), "1".to_owned())];
        env.extend(filter_env);
        Ok(Self {
            untrusted: true,
            args,
            env,
        })
    }
}

/// The installed Git's version, read once.
async fn git_version(workspace: &Path) -> Option<(u32, u32)> {
    static VERSION: tokio::sync::OnceCell<Option<(u32, u32)>> = tokio::sync::OnceCell::const_new();
    *VERSION
        .get_or_init(|| async {
            let output = run_git(workspace, &os_args(&["version"]), 1024)
                .await
                .ok()?;
            filters::version(&output.stdout_text())
        })
        .await
}

/// What every review read passes to Git, trusted or not: no file system
/// monitor, no hooks (a diff that refreshes the index runs
/// `post-index-change`) and no signature check (the repository's gpg
/// program); a person's own `git diff` runs none of them either.
fn read_only_args() -> Vec<OsString> {
    let hooks = if cfg!(windows) {
        "core.hooksPath=NUL"
    } else {
        "core.hooksPath=/dev/null"
    };
    os_args(&[
        "-c",
        "core.fsmonitor=false",
        "-c",
        hooks,
        "-c",
        "log.showSignature=false",
    ])
}

/// A folder that is not one: Git then finds no translations and speaks
/// English, so its messages can be recognised in any locale, while the
/// programs it runs keep the person's locale.
const NO_TRANSLATIONS: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

async fn run_git(workspace: &Path, args: &[OsString], stdout_limit: usize) -> Result<GitOutput> {
    run_git_with(workspace, args, stdout_limit, &Guard::default()).await
}

async fn run_git_with(
    workspace: &Path,
    args: &[OsString],
    stdout_limit: usize,
    guard: &Guard,
) -> Result<GitOutput> {
    let mut command = Command::new("git");
    command
        .args(read_only_args())
        .args(&guard.args)
        .args(args)
        .current_dir(workspace)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TEXTDOMAINDIR", NO_TRANSLATIONS)
        .envs(guard.env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start Git in workspace {}", workspace.display()))?;
    let stdout = child
        .stdout
        .take()
        .context("Git child process did not expose stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("Git child process did not expose stderr")?;
    let stdout_task = tokio::spawn(read_bounded(stdout, stdout_limit));
    let stderr_task = tokio::spawn(read_bounded(stderr, MAX_STDERR_BYTES));

    let status = match timeout(PROCESS_TIMEOUT, child.wait()).await {
        Ok(result) => result.context("failed waiting for Git")?,
        Err(_) => {
            let _ = child.start_kill();
            let _ = timeout(PROCESS_KILL_GRACE, child.wait()).await;
            stdout_task.abort();
            stderr_task.abort();
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            bail!(
                "Git command timed out after {} seconds in {}",
                PROCESS_TIMEOUT.as_secs(),
                workspace.display()
            );
        }
    };

    let stdout = stdout_task
        .await
        .context("Git stdout reader task failed")?
        .context("failed reading Git stdout")?;
    let stderr = stderr_task
        .await
        .context("Git stderr reader task failed")?
        .context("failed reading Git stderr")?;
    Ok(GitOutput {
        status,
        stdout,
        stderr,
    })
}

async fn read_bounded<R>(mut reader: R, limit: usize) -> io::Result<BoundedBytes>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(BoundedBytes {
                bytes,
                truncated: false,
            });
        }

        let remaining = limit.saturating_sub(bytes.len());
        if read > remaining {
            bytes.extend_from_slice(&buffer[..remaining]);
            return Ok(BoundedBytes {
                bytes,
                truncated: true,
            });
        }
        bytes.extend_from_slice(&buffer[..read]);

        if bytes.len() == limit {
            let mut probe = [0u8; 1];
            let truncated = reader.read(&mut probe).await? != 0;
            return Ok(BoundedBytes { bytes, truncated });
        }
    }
}

fn os_args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn safe_relative_path(file: &str) -> bool {
    let path = Path::new(file);
    !file.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

#[derive(Debug)]
struct Numstat {
    file: String,
    additions: u64,
    deletions: u64,
    binary: bool,
}

fn parse_numstat_records(bytes: &[u8]) -> Vec<Numstat> {
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let first = record.iter().position(|byte| *byte == b'\t')?;
            let second = record[first + 1..].iter().position(|byte| *byte == b'\t')? + first + 1;
            let additions = &record[..first];
            let deletions = &record[first + 1..second];
            let file = String::from_utf8(record[second + 1..].to_vec()).ok()?;
            if !safe_relative_path(&file) {
                return None;
            }
            let binary = additions == b"-" || deletions == b"-";
            Some(Numstat {
                file,
                additions: parse_count(additions),
                deletions: parse_count(deletions),
                binary,
            })
        })
        .collect()
}

fn parse_count(value: &[u8]) -> u64 {
    if value == b"-" {
        return 0;
    }
    std::str::from_utf8(value)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn parse_status_records(bytes: &[u8]) -> Vec<(String, FileDiffStatus)> {
    let fields: Vec<_> = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect();
    fields
        .chunks_exact(2)
        .filter_map(|fields| {
            let code = fields[0];
            let file = String::from_utf8(fields[1].to_vec()).ok()?;
            if !safe_relative_path(&file) {
                return None;
            }
            let status = match code.first().copied() {
                Some(b'A') => FileDiffStatus::Added,
                Some(b'D') => FileDiffStatus::Deleted,
                _ => FileDiffStatus::Modified,
            };
            Some((file, status))
        })
        .collect()
}

fn nul_utf8_paths(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .filter_map(|field| String::from_utf8(field.to_vec()).ok())
        .filter(|file| safe_relative_path(file))
        .collect()
}

fn split_git_patch(patch: &str, truncated: bool) -> Vec<&str> {
    let mut starts = Vec::new();
    if patch.starts_with("diff --git ") {
        starts.push(0);
    }
    starts.extend(
        patch
            .match_indices("\ndiff --git ")
            .map(|(index, _)| index + 1),
    );
    let mut chunks: Vec<_> = starts
        .iter()
        .enumerate()
        .map(|(index, start)| &patch[*start..starts.get(index + 1).copied().unwrap_or(patch.len())])
        .collect();
    if truncated {
        chunks.pop();
    }
    chunks
}

fn file_from_patch_chunk(chunk: &str) -> Option<String> {
    for line in chunk.lines() {
        if let Some(path) = line.strip_prefix("+++ ")
            && let Some(file) = file_from_diff_path(path)
        {
            return Some(file);
        }
    }
    for line in chunk.lines() {
        if let Some(path) = line.strip_prefix("--- ")
            && let Some(file) = file_from_diff_path(path)
        {
            return Some(file);
        }
    }
    chunk
        .lines()
        .find_map(|line| line.strip_prefix("diff --git "))
        .and_then(file_from_git_header)
}

fn file_from_diff_path(value: &str) -> Option<String> {
    let value = if value.starts_with('"') {
        parse_quoted_path(value)?.0
    } else {
        value.split('\t').next()?.to_owned()
    };
    if value == "/dev/null" {
        return None;
    }
    let file = value
        .strip_prefix("a/")
        .or_else(|| value.strip_prefix("b/"))
        .unwrap_or(&value)
        .to_owned();
    safe_relative_path(&file).then_some(file)
}

fn file_from_git_header(header: &str) -> Option<String> {
    if header.starts_with('"') {
        let (_, end) = parse_quoted_path(header)?;
        return file_from_diff_path(header[end..].trim_start());
    }
    let separator = header.find(" b/")?;
    file_from_diff_path(&header[separator + 1..])
}

fn parse_quoted_path(value: &str) -> Option<(String, usize)> {
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut output = Vec::new();
    let mut index = 1usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return String::from_utf8(output).ok().map(|path| (path, index + 1)),
            b'\\' => {
                index += 1;
                let escaped = *bytes.get(index)?;
                match escaped {
                    b'a' => output.push(0x07),
                    b'b' => output.push(0x08),
                    b't' => output.push(b'\t'),
                    b'n' => output.push(b'\n'),
                    b'v' => output.push(0x0b),
                    b'f' => output.push(0x0c),
                    b'r' => output.push(b'\r'),
                    b'0'..=b'7' => {
                        let mut octal = u16::from(escaped - b'0');
                        let mut consumed = 0;
                        while consumed < 2 {
                            let Some(next @ b'0'..=b'7') = bytes.get(index + 1).copied() else {
                                break;
                            };
                            index += 1;
                            consumed += 1;
                            octal = octal * 8 + u16::from(next - b'0');
                        }
                        output.push(u8::try_from(octal).ok()?);
                    }
                    _ => output.push(escaped),
                }
            }
            byte => output.push(byte),
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
        process::Command as StdCommand,
    };

    use super::*;

    /// The review of a trusted project, the default.
    async fn working_tree_diff(workspace: &Path) -> Result<Vec<ReviewFile>> {
        review(workspace, DiffMode::WorkingTree, true).await
    }

    async fn branch_diff(workspace: &Path) -> Result<Vec<ReviewFile>> {
        review(workspace, DiffMode::Branch, true).await
    }

    async fn review(workspace: &Path, mode: DiffMode, trusted: bool) -> Result<Vec<ReviewFile>> {
        diff_selected(workspace, mode, DEFAULT_CONTEXT_LINES, false, None, trusted).await
    }

    async fn detect_default_branch(workspace: &Path) -> Result<Option<DefaultBranch>> {
        ensure_worktree(workspace).await?;
        detect_default_branch_inner(workspace).await
    }

    struct TestRepo {
        path: PathBuf,
    }

    impl TestRepo {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "haarness-vcs-{}-{}",
                std::process::id(),
                uuid::Uuid::now_v7()
            ));
            fs::create_dir_all(&path).expect("create test repository");
            run(&path, &["init", "-b", "main"]);
            run(&path, &["config", "user.email", "vcs-test@haarness.local"]);
            run(&path, &["config", "user.name", "Haarness VCS Test"]);
            Self { path }
        }

        fn write(&self, relative: &str, contents: impl AsRef<[u8]>) {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(path, contents).expect("write fixture");
        }

        fn commit_all(&self, message: &str) {
            run(&self.path, &["add", "--all"]);
            run(&self.path, &["commit", "-m", message]);
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn run(repo: &Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn sorted_names(diffs: &HashMap<String, ReviewFile>) -> Vec<&str> {
        let mut names: Vec<_> = diffs.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    fn by_file(diffs: Vec<ReviewFile>) -> HashMap<String, ReviewFile> {
        diffs
            .into_iter()
            .map(|diff| (diff.diff.file.clone(), diff))
            .collect()
    }

    #[tokio::test]
    async fn working_tree_includes_staged_unstaged_deleted_and_untracked() {
        let repo = TestRepo::new();
        repo.write("modified.txt", "before\n");
        repo.write("deleted.txt", "delete me\n");
        repo.commit_all("baseline");

        repo.write("modified.txt", "after\n");
        repo.write("staged.txt", "staged\n");
        run(&repo.path, &["add", "staged.txt"]);
        fs::remove_file(repo.path.join("deleted.txt")).expect("delete fixture");
        repo.write("untracked.txt", "untracked\n");

        let diffs = by_file(working_tree_diff(&repo.path).await.expect("working diff"));
        assert_eq!(diffs["modified.txt"].diff.status, FileDiffStatus::Modified);
        assert_eq!(diffs["deleted.txt"].diff.status, FileDiffStatus::Deleted);
        assert_eq!(diffs["staged.txt"].diff.status, FileDiffStatus::Added);
        assert_eq!(diffs["untracked.txt"].diff.status, FileDiffStatus::Added);
        assert_eq!(diffs["untracked.txt"].diff.additions, 1);
        assert!(diffs.values().all(|diff| diff.diff.patch.is_some()));
    }

    #[tokio::test]
    async fn branch_diff_uses_default_branch_merge_base_and_live_changes() {
        let repo = TestRepo::new();
        repo.write("base.txt", "base\n");
        repo.commit_all("baseline");
        run(&repo.path, &["checkout", "-b", "feature"]);
        repo.write("committed.txt", "feature\n");
        repo.commit_all("feature change");
        repo.write("base.txt", "live\n");
        repo.write("untracked.txt", "live untracked\n");

        let base = detect_default_branch(&repo.path)
            .await
            .expect("detect default")
            .expect("default branch");
        assert_eq!(base.name, "main");
        let diffs = by_file(branch_diff(&repo.path).await.expect("branch diff"));
        assert!(diffs.contains_key("committed.txt"));
        assert!(diffs.contains_key("base.txt"));
        assert!(diffs.contains_key("untracked.txt"));
    }

    #[tokio::test]
    async fn unborn_repository_compares_index_to_empty_tree() {
        let repo = TestRepo::new();
        repo.write("staged.txt", "staged\n");
        run(&repo.path, &["add", "staged.txt"]);
        repo.write("untracked.txt", "untracked\n");

        let diffs = by_file(working_tree_diff(&repo.path).await.expect("unborn diff"));
        assert_eq!(diffs["staged.txt"].diff.status, FileDiffStatus::Added);
        assert_eq!(diffs["untracked.txt"].diff.status, FileDiffStatus::Added);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subdirectory_scope_odd_names_and_symlinks_are_safe() {
        use std::os::unix::fs::symlink;

        let repo = TestRepo::new();
        repo.write("outside.txt", "before\n");
        repo.write("scope/inside.txt", "before\n");
        repo.commit_all("baseline");
        repo.write("outside.txt", "outside changed\n");
        repo.write("scope/inside.txt", "inside changed\n");
        repo.write("scope/tab\tline\nquote\".txt", "odd\n");
        repo.write("secret.txt", "DO_NOT_LEAK_TARGET_CONTENT\n");
        symlink("../secret.txt", repo.path.join("scope/link.txt")).expect("create symlink");

        let scope = repo.path.join("scope");
        let diffs = by_file(working_tree_diff(&scope).await.expect("subdirectory diff"));
        assert!(diffs.contains_key("inside.txt"));
        assert!(diffs.contains_key("tab\tline\nquote\".txt"));
        assert!(!diffs.contains_key("outside.txt"));
        assert!(!diffs.contains_key("secret.txt"));
        let link_patch = diffs["link.txt"]
            .diff
            .patch
            .as_deref()
            .expect("symlink patch");
        assert!(link_patch.contains("../secret.txt"));
        assert!(!link_patch.contains("DO_NOT_LEAK_TARGET_CONTENT"));
    }

    #[tokio::test]
    async fn large_untracked_patch_is_metadata_only() {
        let repo = TestRepo::new();
        repo.write("base.txt", "base\n");
        repo.commit_all("baseline");
        repo.write("huge.txt", vec![b'x'; MAX_PATCH_BYTES + 1024]);

        let diffs = by_file(working_tree_diff(&repo.path).await.expect("large diff"));
        let huge = &diffs["huge.txt"];
        assert_eq!(huge.diff.status, FileDiffStatus::Added);
        assert_eq!(huge.diff.additions, 1);
        assert!(huge.diff.patch.is_none());
        assert!(huge.diff.truncated);
        assert_eq!(huge.truncation, Some(DiffTruncation::FileTooLarge));
    }

    #[tokio::test]
    async fn aggregate_patch_bodies_never_exceed_response_budget() {
        let repo = TestRepo::new();
        repo.write("base.txt", "base\n");
        repo.commit_all("baseline");
        repo.write("first.txt", vec![b'a'; 6_000_000]);
        repo.write("second.txt", vec![b'b'; 6_000_000]);

        let diffs = working_tree_diff(&repo.path).await.expect("aggregate diff");
        let returned_bytes: usize = diffs
            .iter()
            .filter_map(|diff| diff.diff.patch.as_ref())
            .map(String::len)
            .sum();
        assert!(returned_bytes <= MAX_TOTAL_PATCH_BYTES);
        assert_eq!(diffs.len(), 2);
        assert!(diffs.iter().any(|diff| diff.diff.truncated));
    }
    #[tokio::test]
    async fn deferred_summary_matches_eager_metadata_and_paths_are_literal() {
        let repo = TestRepo::new();
        repo.write("a[1].txt", "old\n");
        repo.write("a1.txt", "other\n");
        repo.write("deleted.txt", "delete\n");
        repo.commit_all("baseline");
        repo.write("a[1].txt", "new\nextra\n");
        repo.write("a1.txt", "changed\n");
        fs::remove_file(repo.path.join("deleted.txt")).unwrap();
        repo.write("new *.txt", "untracked\n");
        repo.write("binary", [0, 1, 2]);
        for mode in [DiffMode::WorkingTree, DiffMode::Branch] {
            let eager = diff_selected(&repo.path, mode, 4, false, None, true)
                .await
                .unwrap();
            let summary = diff_selected(&repo.path, mode, 4, true, None, true)
                .await
                .unwrap();
            let stripped: Vec<_> = eager
                .iter()
                .cloned()
                .map(|mut file| {
                    file.diff.patch = None;
                    file
                })
                .collect();
            assert_eq!(summary, stripped);
            for file in &eager {
                let detail = diff_selected(&repo.path, mode, 4, false, Some(&file.diff.file), true)
                    .await
                    .unwrap();
                assert_eq!(detail, vec![file.clone()]);
            }
        }
        for path in ["../outside", "/absolute", "", ":(glob)*"] {
            let result = diff_selected(
                &repo.path,
                DiffMode::WorkingTree,
                4,
                false,
                Some(path),
                true,
            )
            .await;
            assert!(result.is_err() || result.unwrap().is_empty());
        }
        assert!(
            diff_selected(
                &repo.path,
                DiffMode::WorkingTree,
                4,
                false,
                Some("src"),
                true
            )
            .await
            .unwrap()
            .is_empty()
        );
    }

    #[tokio::test]
    async fn deferred_file_gets_its_own_patch_budget_and_refreshes() {
        let repo = TestRepo::new();
        repo.write("base", "base\n");
        repo.commit_all("baseline");
        repo.write("first", vec![b'a'; 6_000_000]);
        repo.write("second", vec![b'b'; 6_000_000]);
        let summary = diff_selected(&repo.path, DiffMode::WorkingTree, 4, true, None, true)
            .await
            .unwrap();
        assert_eq!(summary.len(), 2);
        assert!(
            summary
                .iter()
                .all(|file| file.diff.patch.is_none() && !file.diff.truncated)
        );
        let detail = diff_selected(
            &repo.path,
            DiffMode::WorkingTree,
            4,
            false,
            Some("second"),
            true,
        )
        .await
        .unwrap();
        assert!(!detail[0].diff.truncated);
        assert!(detail[0].diff.patch.as_ref().unwrap().len() > 6_000_000);
        repo.write("second", "fresh\n");
        let detail = diff_selected(
            &repo.path,
            DiffMode::WorkingTree,
            4,
            false,
            Some("second"),
            true,
        )
        .await
        .unwrap();
        assert!(detail[0].diff.patch.as_ref().unwrap().contains("+fresh"));
    }

    /// A working tree that is a repository rather than a review: more files
    /// than the bound, an over-sized file, binary content, and paths the
    /// ignore rules exclude. The response must stay bounded, keep saying what
    /// it left out, and never mention an ignored path.
    #[tokio::test]
    async fn oversized_working_tree_is_bounded_and_says_so() {
        let repo = TestRepo::new();
        repo.write(".gitignore", "ignored/\n*.log\n");
        repo.write("base.txt", "base\n");
        repo.commit_all("baseline");

        let untracked = MAX_FILES + 200;
        for index in 0..untracked {
            // `zz/` sorts last, so the bound has to cut these rather than
            // the interesting files this test then looks for.
            repo.write(&format!("zz/{index:05}.txt"), "line\n");
        }
        repo.write("huge.txt", vec![b'x'; MAX_PATCH_BYTES + 1024]);
        repo.write("picture.bin", [b'P', b'N', b'G', 0, 1, 2, 3]);
        for index in 0..50 {
            repo.write(&format!("ignored/{index}.txt"), "ignored\n");
            repo.write(&format!("noisy{index}.log"), "ignored\n");
        }

        let started = std::time::Instant::now();
        let diffs = working_tree_diff(&repo.path).await.expect("bounded diff");
        let elapsed = started.elapsed();

        assert_eq!(
            diffs.len(),
            MAX_FILES,
            "the file bound is what caps the list"
        );
        let omitted = diffs[0].omitted_files.expect("omitted count");
        assert_eq!(
            u64::try_from(untracked + 2 - MAX_FILES).unwrap(),
            omitted,
            "the count of dropped paths travels on every entry"
        );
        assert!(
            diffs.iter().all(|file| file.omitted_files == Some(omitted)),
            "every entry repeats the same count"
        );
        assert!(
            !diffs
                .iter()
                .any(|file| file.diff.file.starts_with("ignored/")
                    || file.diff.file.starts_with("noisy")),
            "ignored paths never enter a review"
        );

        let by_path = by_file(diffs);
        let huge = &by_path["huge.txt"];
        assert!(huge.diff.truncated && huge.diff.patch.is_none());
        assert_eq!(huge.truncation, Some(DiffTruncation::FileTooLarge));
        assert_eq!(huge.diff.additions, 1, "counts survive a dropped body");

        let binary = &by_path["picture.bin"];
        assert!(binary.diff.binary && binary.diff.patch.is_none());
        assert_eq!(binary.truncation, Some(DiffTruncation::Binary));

        let body_bytes: usize = by_path
            .values()
            .filter_map(|file| file.diff.patch.as_ref())
            .map(String::len)
            .sum();
        assert!(body_bytes <= MAX_TOTAL_PATCH_BYTES);
        // The old path spawned two `git diff --no-index` processes per
        // untracked file, one after another. Anything near that is a
        // regression back into it, whatever the machine.
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "bounded review took {elapsed:?}"
        );
    }

    /// The synthesized patch for a new file has to be a patch a viewer can
    /// parse: real hunk header, one `+` per line, and Git's missing-newline
    /// marker where the file has no final newline.
    #[tokio::test]
    async fn untracked_patches_are_well_formed_unified_diffs() {
        let repo = TestRepo::new();
        repo.write("base.txt", "base\n");
        repo.commit_all("baseline");
        repo.write("added.txt", "one\ntwo\nthree\n");
        repo.write("partial.txt", "no trailing newline");
        repo.write("empty.txt", "");

        let diffs = by_file(working_tree_diff(&repo.path).await.expect("new files"));

        let added = &diffs["added.txt"];
        assert_eq!(added.diff.additions, 3);
        let patch = added.diff.patch.as_deref().expect("patch");
        assert!(patch.starts_with("diff --git a/added.txt b/added.txt\n"));
        assert!(patch.contains("\n--- /dev/null\n+++ b/added.txt\n"));
        assert!(patch.contains("@@ -0,0 +1,3 @@\n+one\n+two\n+three\n"));
        assert_eq!(added_lines(patch), 3);

        let partial = &diffs["partial.txt"];
        assert_eq!(partial.diff.additions, 1);
        let patch = partial.diff.patch.as_deref().expect("patch");
        assert!(patch.contains("@@ -0,0 +1 @@\n+no trailing newline\n"));
        assert!(patch.ends_with("\\ No newline at end of file\n"));

        let empty = &diffs["empty.txt"];
        assert_eq!(empty.diff.additions, 0);
        assert!(empty.diff.patch.is_none() && !empty.diff.truncated);
    }

    /// A folder outside the repository where the programs a repository's
    /// configuration names leave a mark when they run.
    struct Marks(PathBuf);

    impl Marks {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("haarness-vcs-marks-{}", uuid::Uuid::now_v7()));
            fs::create_dir_all(&path).expect("create marks folder");
            Self(path)
        }

        /// A shell command that leaves the mark `name`, then runs `then`.
        fn command(&self, name: &str, then: &str) -> String {
            format!("touch '{}'; {then}", self.0.join(name).display())
        }

        fn left(&self) -> Vec<String> {
            let mut names: Vec<_> = fs::read_dir(&self.0)
                .expect("read marks")
                .map(|entry| {
                    entry
                        .expect("mark")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Marks {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Gives `relative` a new modification time, so Git refreshes its entry
    /// in the index (and writes the index) though its content is unchanged.
    fn touch_later(repo: &Path, relative: &str) {
        fs::File::options()
            .write(true)
            .open(repo.join(relative))
            .expect("open fixture")
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(3600))
            .expect("set modification time");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_review_runs_no_program_a_person_s_git_diff_would_not_and_untrusted_none() {
        use std::os::unix::fs::PermissionsExt;

        let marks = Marks::new();
        let repo = TestRepo::new();
        // `filter=` selects the filter with the empty name, `[filter ""]`.
        repo.write(".gitattributes", "*.txt filter=evil\n*.dat filter=\n");
        repo.write("a.txt", "one\n");
        repo.write("b.dat", "bbb\n");
        repo.write("same.md", "same\n");
        repo.commit_all("baseline");
        let config = repo.path.join(".git/config");
        let mut text = fs::read_to_string(&config).unwrap();
        let _ = write!(
            text,
            "[filter \"\"]\n\tclean = \"{}\"\n",
            marks.command("empty", "cat")
        );
        fs::write(&config, text).unwrap();
        run(
            &repo.path,
            &[
                "config",
                "filter.evil.clean",
                &marks.command("clean", "cat"),
            ],
        );
        run(&repo.path, &["config", "filter.evil.required", "true"]);
        let monitor = repo.path.join(".git/monitor.sh");
        fs::write(
            &monitor,
            format!("#!/bin/sh\n{}\n", marks.command("fsmonitor", "exit 1")),
        )
        .unwrap();
        fs::set_permissions(&monitor, fs::Permissions::from_mode(0o755)).unwrap();
        run(
            &repo.path,
            &["config", "core.fsmonitor", &monitor.to_string_lossy()],
        );
        let hook = repo.path.join(".git/hooks/post-index-change");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(
            &hook,
            format!("#!/bin/sh\n{}\n", marks.command("hook", "true")),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        repo.write("a.txt", "two\n");
        repo.write("b.dat", "BBB\n");
        touch_later(&repo.path, "same.md");

        let untrusted = by_file(
            review(&repo.path, DiffMode::WorkingTree, false)
                .await
                .expect("untrusted review"),
        );
        assert_eq!(sorted_names(&untrusted), ["a.txt", "b.dat"]);
        assert!(
            untrusted["a.txt"]
                .diff
                .patch
                .as_deref()
                .unwrap()
                .contains("+two")
        );
        assert!(marks.left().is_empty(), "{:?}", marks.left());
        // A trusted project reads as the person's own `git diff`: its filters
        // run, while the monitor and the hooks still do not.
        touch_later(&repo.path, "same.md");
        let trusted = by_file(working_tree_diff(&repo.path).await.expect("trusted review"));
        assert_eq!(sorted_names(&trusted), ["a.txt", "b.dat"]);
        assert_eq!(marks.left(), ["clean", "empty"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_untrusted_review_does_not_run_git_inside_a_changed_submodule() {
        let marks = Marks::new();
        let module = TestRepo::new();
        module.write(".gitattributes", "*.txt filter=inner\n");
        module.write("inner.txt", "inner\n");
        module.commit_all("module");
        let repo = TestRepo::new();
        repo.write("a.txt", "one\n");
        repo.commit_all("baseline");
        run(
            &repo.path,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                &module.path.to_string_lossy(),
                "module",
            ],
        );
        repo.commit_all("add module");
        let inside = repo.path.join("module");
        run(
            &inside,
            &[
                "config",
                "filter.inner.clean",
                &marks.command("submodule", "cat"),
            ],
        );
        // The same size, so Git reads the file (through the filter) to see
        // whether it changed.
        fs::write(inside.join("inner.txt"), "INNER\n").unwrap();
        repo.write("a.txt", "two\n");

        let untrusted = by_file(
            review(&repo.path, DiffMode::WorkingTree, false)
                .await
                .expect("untrusted review"),
        );
        assert!(untrusted.contains_key("a.txt"));
        assert!(marks.left().is_empty(), "{:?}", marks.left());
        // A trusted project's review looks inside, as `git diff` does.
        working_tree_diff(&repo.path).await.expect("trusted review");
        assert_eq!(marks.left(), ["submodule"]);
    }

    #[tokio::test]
    async fn an_untrusted_review_of_a_partial_clone_fetches_nothing() {
        let marks = Marks::new();
        let origin = TestRepo::new();
        run(&origin.path, &["config", "uploadpack.allowFilter", "true"]);
        origin.write("file.txt", "one\n");
        origin.commit_all("one");
        run(&origin.path, &["checkout", "-q", "-b", "feature"]);
        origin.write("file.txt", "two\n");
        origin.commit_all("two");
        run(&origin.path, &["checkout", "-q", "main"]);
        let clone = origin.path.with_extension("partial");
        let output = StdCommand::new("git")
            .args(["clone", "-q", "--filter=blob:none", "--branch", "feature"])
            .arg(format!("file://{}", origin.path.display()))
            .arg(&clone)
            .output()
            .expect("clone");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        run(
            &clone,
            &[
                "config",
                "remote.origin.uploadpack",
                &marks.command("fetch", "git-upload-pack"),
            ],
        );

        // The base's copy of the file was never downloaded, and the review
        // of an untrusted project does not fetch it; it says what to do
        // instead of quoting Git.
        let untrusted = review(&clone, DiffMode::Branch, false).await;
        assert!(marks.left().is_empty(), "{:?} {untrusted:?}", marks.left());
        assert_eq!(untrusted.unwrap_err().to_string(), MISSING_OBJECTS);
        // A trusted project's review fetches it, as `git diff` would.
        let trusted = branch_diff(&clone).await;
        let _ = fs::remove_dir_all(&clone);
        let trusted = by_file(trusted.expect("trusted review"));
        assert!(trusted.contains_key("file.txt"));
        assert_eq!(marks.left(), ["fetch"]);
    }

    #[tokio::test]
    async fn git_tells_the_review_its_errors_in_english_in_any_locale() {
        let folder =
            std::env::temp_dir().join(format!("haarness-vcs-locale-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&folder).unwrap();
        let german = [("LC_ALL", "de_DE.UTF-8"), ("LANGUAGE", "de")];
        let guard = Guard {
            env: german
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            ..Guard::default()
        };
        let output = run_git_with(&folder, &os_args(&["status"]), 4096, &guard)
            .await
            .unwrap();
        // Without the review's setting, a Git with its German catalog says
        // "Kein Git-Repository" here.
        let plain = StdCommand::new("git")
            .arg("status")
            .current_dir(&folder)
            .envs(german)
            .output()
            .unwrap();
        let _ = fs::remove_dir_all(&folder);
        assert!(
            output.stderr_text().contains("not a git repository"),
            "{} (plain Git: {})",
            output.stderr_text(),
            String::from_utf8_lossy(&plain.stderr)
        );
    }

    #[tokio::test]
    async fn names_and_text_outside_ascii_read_the_same_trusted_or_not() {
        let repo = TestRepo::new();
        repo.write("café.txt", "avant\n");
        repo.write("日本/ファイル.md", "一\n");
        repo.commit_all("baseline");
        repo.write("café.txt", "après\n");
        repo.write("日本/ファイル.md", "二\n");
        repo.write("新しい.txt", "ü\n");
        for trusted in [true, false] {
            let diffs = by_file(
                review(&repo.path, DiffMode::WorkingTree, trusted)
                    .await
                    .expect("review"),
            );
            let mut names = diffs.keys().cloned().collect::<Vec<_>>();
            names.sort();
            assert_eq!(
                names,
                ["café.txt", "新しい.txt", "日本/ファイル.md"],
                "{trusted}"
            );
            assert!(
                diffs["café.txt"]
                    .diff
                    .patch
                    .as_deref()
                    .unwrap()
                    .contains("+après")
            );
            assert!(
                diffs["日本/ファイル.md"]
                    .diff
                    .patch
                    .as_deref()
                    .unwrap()
                    .contains("-一\n+二")
            );
        }
    }

    fn added_lines(patch: &str) -> usize {
        patch
            .lines()
            .skip_while(|line| !line.starts_with("@@"))
            .filter(|line| line.starts_with('+'))
            .count()
    }
}
