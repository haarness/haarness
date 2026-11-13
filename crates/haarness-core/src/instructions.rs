//! Instruction-file discovery and `@import` expansion shared by the
//! composition root (the system-level `AGENTS.md` family) and the `read`
//! tool (lazy discovery of nested instruction files).
//!
//! Import limits: five import hops, twenty files and
//! 200 KiB per assembly, 40 KiB per file. References inside fenced code
//! blocks and inline code spans stay literal.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Largest instruction file that is inlined; bigger files are skipped, by
/// their size on disk, without being read.
pub const MAX_FILE_BYTES: usize = 40 * 1024;
/// Maximum `@import` nesting depth.
pub const MAX_DEPTH: usize = 5;

/// Cross-file limits for one `@import` assembly.
#[derive(Debug, Clone)]
pub struct ImportBudget {
    pub files: usize,
    pub bytes: usize,
}

impl Default for ImportBudget {
    fn default() -> Self {
        Self {
            files: 20,
            bytes: 200 * 1024,
        }
    }
}

/// Instruction family names in precedence order. Without Claude Code
/// compatibility only `AGENTS.md` is considered, which keeps the default
/// behaviour identical to earlier releases.
#[must_use]
pub fn family(claude: bool) -> &'static [&'static str] {
    if claude {
        &["AGENTS.md", "CLAUDE.md", ".claude/CLAUDE.md", "CONTEXT.md"]
    } else {
        &["AGENTS.md"]
    }
}

fn local_sibling(name: &str) -> String {
    name.strip_suffix(".md").map_or_else(
        || format!("{name}.local"),
        |stem| format!("{stem}.local.md"),
    )
}

/// Instruction files in `dir`: the first family member that exists wins,
/// together with its `.local.md` sibling under Claude Code compatibility.
#[must_use]
pub fn find_in(dir: &Path, claude: bool) -> Vec<PathBuf> {
    for name in family(claude) {
        let mut found = Vec::new();
        let primary = dir.join(name);
        if primary.is_file() {
            found.push(primary);
        }
        if claude {
            let local = dir.join(local_sibling(name));
            if local.is_file() {
                found.push(local);
            }
        }
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// Instruction files that apply to `file` but are not part of the system
/// prompt: every family match from the file's directory up to, excluding,
/// the workspace root. Nearest directory first. Files outside the workspace
/// yield nothing.
#[must_use]
pub fn nested(file: &Path, workspace: &Path, claude: bool) -> Vec<PathBuf> {
    let root = crate::paths::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let target = crate::paths::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let mut out = Vec::new();
    let mut current = target.parent();
    while let Some(dir) = current {
        if dir == root || !dir.starts_with(&root) {
            break;
        }
        out.extend(
            find_in(dir, claude)
                .into_iter()
                .filter(|candidate| candidate != &target),
        );
        current = dir.parent();
    }
    out
}

/// The nested instruction files one turn's model can see: those attached to
/// a `read` result still in its context. The turn sets it from the history
/// each provider round sends and that round's reads add to it, so a file is
/// attached once while it stays in view, and again after compaction or an
/// undo removed the result that carried it.
pub type InContext = std::sync::Arc<parking_lot::Mutex<std::collections::HashSet<PathBuf>>>;

/// The instruction files the tool results among `items` carried, also for a
/// `read` inside a batch, keyed as `read` keys them: by the file each
/// resolves to (`loadedResolved`), so an `AGENTS.md` that is a symlink or
/// spelled in another case stays in view. A result stored before reads
/// recorded that names them as `loaded` does. Nothing here touches the disk.
#[must_use]
pub fn attached<'a>(
    items: impl IntoIterator<Item = &'a haarness_protocol::Item>,
) -> std::collections::HashSet<PathBuf> {
    let mut found = std::collections::HashSet::new();
    let mut add = |metadata: &serde_json::Value| {
        for path in metadata
            .get("loadedResolved")
            .or_else(|| metadata.get("loaded"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            found.insert(PathBuf::from(path));
        }
    };
    for item in items {
        let haarness_protocol::ItemData::ToolResult { metadata, .. } = &item.data else {
            continue;
        };
        add(metadata);
        for step in metadata
            .get("results")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            add(&step["metadata"]);
        }
    }
    found
}

/// Wrap one instruction body for the system prompt or a tool result.
#[must_use]
pub fn segment(source: &str, content: &str) -> String {
    format!(
        "<instructions source=\"{source}\">\n{}\n</instructions>",
        content.trim()
    )
}

/// Blank fenced blocks (``` or ~~~) and single-line inline code spans with
/// spaces of equal byte length, so offsets found in the result index the
/// original text.
#[must_use]
pub fn mask_code(content: &str) -> String {
    let mut bytes = content.as_bytes().to_vec();
    let mut fence: Option<&[u8]> = None;
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some(&b"```"[..])
        } else if trimmed.starts_with("~~~") {
            Some(&b"~~~"[..])
        } else {
            None
        };
        match (fence, marker) {
            (Some(open), Some(close)) if open == close => {
                fence = None;
                blank(&mut bytes, start, start + line.len());
                continue;
            }
            (Some(_), _) => {
                blank(&mut bytes, start, start + line.len());
                continue;
            }
            (None, Some(open)) => {
                fence = Some(open);
                blank(&mut bytes, start, start + line.len());
                continue;
            }
            (None, None) => {}
        }
        mask_spans(&mut bytes, start, start + line.len());
    }
    String::from_utf8(bytes).expect("masking only writes ASCII spaces")
}

fn blank(bytes: &mut [u8], start: usize, end: usize) {
    for byte in &mut bytes[start..end] {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
}

fn mask_spans(bytes: &mut [u8], start: usize, end: usize) {
    let mut index = start;
    while index < end {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let run_start = index;
        while index < end && bytes[index] == b'`' {
            index += 1;
        }
        let run = index - run_start;
        // Find a closing run of exactly the same length on this line.
        let mut cursor = index;
        let mut closed = None;
        while cursor < end {
            if bytes[cursor] != b'`' {
                cursor += 1;
                continue;
            }
            let candidate = cursor;
            while cursor < end && bytes[cursor] == b'`' {
                cursor += 1;
            }
            if cursor - candidate == run {
                closed = Some(cursor);
                break;
            }
        }
        if let Some(close) = closed {
            blank(bytes, run_start, close);
            index = close;
        }
    }
}

/// One `@path` reference found in instruction or template text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// Byte offset of the `@`.
    pub start: usize,
    /// Byte offset one past the reference.
    pub end: usize,
    /// The referenced path as written, without the `@` or its quotes.
    pub path: String,
    /// `true` for `@"…"`, `@'…'` and `@<…>`.
    pub quoted: bool,
}

/// `@path` references outside code: the quoted forms `@"a b"`, `@'a b'` and
/// `@<a b>`, plus bare tokens terminated by whitespace, a backtick or a
/// comma. A `@` preceded by a word character or backtick (an email address
/// or a literal) is not a reference. Trailing sentence punctuation is
/// dropped from bare tokens.
#[must_use]
pub fn references(content: &str) -> Vec<Reference> {
    let masked = mask_code(content);
    let bytes = masked.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'@' {
            index += 1;
            continue;
        }
        let preceded = index > 0
            && (bytes[index - 1].is_ascii_alphanumeric()
                || matches!(bytes[index - 1], b'_' | b'`'));
        if preceded {
            index += 1;
            continue;
        }
        let start = index;
        let rest = &masked[index + 1..];
        // Quoted forms: the closing delimiter must sit on the same line.
        let quoted = rest.chars().next().and_then(|open| {
            let close = match open {
                '"' => '"',
                '\'' => '\'',
                '<' => '>',
                _ => return None,
            };
            let body = &rest[1..];
            let line_end = body.find(['\r', '\n']).unwrap_or(body.len());
            let close_at = body[..line_end].find(close)?;
            (close_at > 0).then_some(close_at)
        });
        if let Some(close_at) = quoted {
            // `@` + opening quote + body + closing quote.
            let end = index + 2 + close_at + 1;
            out.push(Reference {
                start,
                end,
                path: content[start + 2..end - 1].to_owned(),
                quoted: true,
            });
            index = end;
            continue;
        }
        let token_len = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '`' | ','))
            .unwrap_or(rest.len());
        let token = &content[index + 1..index + 1 + token_len];
        let trimmed = token.trim_end_matches(['.', ';', ':', ')', ']', '}']);
        if trimmed.is_empty() {
            index += 1;
            continue;
        }
        out.push(Reference {
            start,
            end: index + 1 + trimmed.len(),
            path: trimmed.to_owned(),
            quoted: false,
        });
        index += 1 + token_len;
    }
    out
}

/// Resolve `reference` against the importing file's directory (`~/` against
/// the home directory, absolute paths as written).
#[must_use]
pub fn locate(reference: &str, base: &Path, home: Option<&Path>) -> PathBuf {
    if let Some(rest) = reference.strip_prefix("~/") {
        if let Some(home) = home {
            return home.join(rest);
        }
    }
    let path = Path::new(reference);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    base.join(path)
}

/// Why an import is left out of the prompt, if it is: the caller's verdict
/// on a file (a `read` rule that denies it) for its real path.
pub type ImportFilter<'a> = &'a dyn Fn(&Path) -> Option<&'static str>;

/// Read one instruction file and resolve its `@path` imports (depth ≤ 5,
/// ≤ 20 files, 40 KiB per file, 200 KiB total). Imported bodies follow the
/// line that referenced them. Targets outside the workspace are only
/// followed when `trusted` (global configuration); references that do not
/// name an existing file are left as written. An import [`left_out`] keeps
/// out is never read: an `[Instruction import omitted: …]` line takes its
/// place, as nothing asks before instructions load. The file itself is
/// judged too, where it really is (a link `AGENTS.md -> …/auth.json`), and
/// only an `[Instruction file omitted: …]` line is returned for it then; a
/// read rule that denies it counts only for a file outside the project
/// that is not `trusted` (one a project's `instructions` names), so the
/// project's and the person's own instruction files load as before.
#[must_use]
pub fn read_with_imports(
    path: &Path,
    workspace: &Path,
    budget: &mut ImportBudget,
    trusted: bool,
    omit: ImportFilter<'_>,
) -> Option<String> {
    let home = haarness_protocol::home_dir();
    let scope = Scope {
        workspace,
        home: home.as_deref(),
        trusted,
        omit,
    };
    if let Some(reason) = file_left_out(path, workspace, trusted, omit) {
        return Some(omitted_file(path, reason));
    }
    resolve(path, &scope, budget, 0)
}

/// The line that stands in the prompt for an instruction file left out.
#[must_use]
pub fn omitted_file(path: &Path, reason: &str) -> String {
    format!(
        "[Instruction file omitted: {} ({reason})]\n",
        path.display()
    )
}

/// Why [`read_with_imports`] leaves the instruction file itself out, if it
/// does, for a notice to the person as well.
#[must_use]
pub fn file_left_out(
    path: &Path,
    workspace: &Path,
    trusted: bool,
    omit: ImportFilter<'_>,
) -> Option<&'static str> {
    let canonical = crate::paths::canonicalize(path).ok()?;
    let root = crate::paths::canonicalize(workspace).ok();
    let inside = root
        .as_deref()
        .is_some_and(|root| canonical.starts_with(root));
    // A project names files outside itself only through its own configuration, which a cloned repository writes:
    // it never gets a private key or another tool's credentials into the prompt, rule or no rule.
    if !trusted && !inside && holds_credentials(&canonical) {
        return Some(CREDENTIALS_FILE);
    }
    let rules: ImportFilter<'_> = if trusted || inside { &|_| None } else { omit };
    // A rule may name the file as it was written rather than where it
    // really is: through a folder link, or Windows' short 8.3 folder names.
    left_out(&canonical, root.as_deref(), rules).or_else(|| {
        (path.is_absolute() && path != canonical)
            .then(|| rules(path))
            .flatten()
    })
}

/// Why [`file_left_out`] leaves out a file that holds credentials.
pub const CREDENTIALS_FILE: &str = "it holds credentials";

/// Why an instruction file over [`MAX_FILE_BYTES`] is left out.
pub const TOO_LARGE: &str = "it is larger than 40 KiB";

/// Why an instruction file is left out once the files before it took the
/// assembly's room ([`ImportBudget`]).
pub const NO_ROOM: &str =
    "the files before it used up the room for instructions (200 KiB, 20 files)";

/// Why [`linked_out`] leaves out a file a link leads out of the project.
pub const LINKED_OUT: &str =
    "a link leads it out of the project to a file that is not a plain Markdown note";

/// The real roots of `workspace`'s project, where a cloned repository puts
/// what it likes: the worktree root (the nearest folder above it, or itself,
/// that holds `.git`) and the workspace itself. Outside Git, the workspace.
#[must_use]
pub fn project_roots(workspace: &Path) -> Vec<PathBuf> {
    let top = workspace
        .ancestors()
        .find(|folder| folder.join(".git").exists())
        .unwrap_or(workspace);
    let mut roots: Vec<PathBuf> = [top, workspace]
        .into_iter()
        .filter_map(|path| crate::paths::canonicalize(path).ok())
        .collect();
    roots.dedup();
    roots
}

/// The person's own instruction folders, where they really are: the global
/// configuration folder and `~/.haarness`, `~/.opencode` and `~/.claude`.
#[must_use]
pub fn personal_folders(config_dir: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    config_dir
        .map(Path::to_path_buf)
        .into_iter()
        .chain(home.into_iter().flat_map(|home| {
            [".haarness", ".opencode", ".claude"]
                .iter()
                .map(move |name| home.join(name))
        }))
        .filter_map(|folder| crate::paths::canonicalize(&folder).ok())
        .collect()
}

/// Why an instruction file the project controls (its own `AGENTS.md`
/// family, a nested one, a match of a relative `instructions` entry) is left
/// out when a link leads it out of the project's `roots`, if it is. A
/// personal note stays welcome (`AGENTS.md -> ~/notes/rules.md`, or a link
/// into `personal`, the person's instruction folders), so only a Markdown
/// file (`.md`, `.mdx`, `.markdown`) whose name does not start with a dot,
/// in no hidden folder but those, may load: a shell history, `.env`,
/// `/proc/self/environ` or a file under `~/.config` never does. The usual
/// checks ([`file_left_out`], [`nested_left_out`]) still follow.
#[must_use]
pub fn linked_out(
    canonical: &Path,
    roots: &[PathBuf],
    personal: &[PathBuf],
) -> Option<&'static str> {
    if roots.iter().any(|root| canonical.starts_with(root)) {
        return None;
    }
    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let markdown = !name.starts_with('.')
        && [".md", ".mdx", ".markdown"]
            .iter()
            .any(|extension| name.ends_with(extension));
    // Hidden folders count below the person's folder that holds it, if any.
    let below = personal
        .iter()
        .find_map(|folder| canonical.strip_prefix(folder).ok())
        .unwrap_or(canonical);
    let hidden = below.parent().is_some_and(|folders| {
        folders.components().any(|part| {
            matches!(part, std::path::Component::Normal(name)
                if name.to_string_lossy().starts_with('.'))
        })
    });
    (!markdown || hidden).then_some(LINKED_OUT)
}

/// Why the instruction assembly will leave `path` out for its size, if it
/// will: larger than [`MAX_FILE_BYTES`], or with no room left in `budget`.
/// The size is the one on disk, so a large file is never read to find out.
/// A file that does not exist, or is not a regular file, gets no reason: the
/// assembly skips it quietly, as it always has.
#[must_use]
pub fn size_left_out(path: &Path, budget: &ImportBudget) -> Option<&'static str> {
    let size = fs::metadata(path).ok().filter(fs::Metadata::is_file)?.len();
    if size > MAX_FILE_BYTES as u64 {
        return Some(TOO_LARGE);
    }
    (budget.files == 0 || size > budget.bytes as u64).then_some(NO_ROOM)
}

/// Why a nested instruction file that a `read` found is left out, if it is,
/// judged where it really is: one a link leads out of the project (its
/// worktree root and `workspace`) must pass [`linked_out`] and hold no
/// credentials; any must be a file Haarness puts in a prompt ([`left_out`],
/// with `omit` judging it both as named and where it leads), and no larger
/// than [`MAX_FILE_BYTES`], measured rather than read.
#[must_use]
pub fn nested_left_out(
    path: &Path,
    workspace: &Path,
    personal: &[PathBuf],
    omit: ImportFilter<'_>,
) -> Option<&'static str> {
    let root = crate::paths::canonicalize(workspace).ok();
    let canonical = crate::paths::canonicalize(path).ok();
    let (Some(root), Some(canonical)) = (root, canonical) else {
        return Some(LINKED_OUT);
    };
    if let Some(reason) = linked_out(&canonical, &project_roots(workspace), personal) {
        return Some(reason);
    }
    if !canonical.starts_with(&root) && holds_credentials(&canonical) {
        return Some(CREDENTIALS_FILE);
    }
    if let Some(reason) = left_out(&canonical, Some(&root), omit)
        .or_else(|| (path != canonical).then(|| omit(path)).flatten())
    {
        return Some(reason);
    }
    let size = fs::metadata(&canonical)
        .ok()
        .filter(fs::Metadata::is_file)
        .map_or(u64::MAX, |metadata| metadata.len());
    (size > MAX_FILE_BYTES as u64).then_some(TOO_LARGE)
}

/// The text of an instruction file of at most [`MAX_FILE_BYTES`], reading no
/// more than that: `None` for a larger file (one that grew after it was
/// measured too), one that is not UTF-8, or one that cannot be read.
#[must_use]
pub fn read_small(path: &Path) -> Option<String> {
    read_within(path, MAX_FILE_BYTES)
}

fn read_within(path: &Path, limit: usize) -> Option<String> {
    use std::io::Read as _;
    // Measured first; and opening a pipe would wait for a writer, so only a
    // regular file is read.
    if !fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= limit as u64)
    {
        return None;
    }
    let mut text = String::new();
    fs::File::open(path)
        .ok()?
        .take(limit as u64 + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() <= limit).then_some(text)
}

/// Files where tools keep keys and credentials: SSH, cloud and cluster tools, package registries, Git, and key
/// files by their extension.
fn holds_credentials(canonical: &Path) -> bool {
    const FOLDERS: [&str; 8] = [
        ".ssh",
        ".aws",
        ".gnupg",
        ".kube",
        ".docker",
        ".azure",
        ".config/gcloud",
        ".config/gh",
    ];
    const NAMES: [&str; 5] = [".netrc", ".npmrc", ".pypirc", ".git-credentials", ".pgpass"];
    const EXTENSIONS: [&str; 6] = ["pem", "key", "p12", "pfx", "jks", "keystore"];
    let in_folder = haarness_protocol::home_dir()
        .and_then(|home| crate::paths::canonicalize(&home).ok())
        .is_some_and(|home| {
            FOLDERS
                .iter()
                .any(|folder| canonical.starts_with(home.join(folder)))
        });
    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    in_folder
        || NAMES.contains(&name.as_str())
        // `name` is lowercased above, so `.PUB` counts as a public key too.
        || (name.starts_with("id_")
            && std::path::Path::new(&name)
                .extension()
                .is_none_or(|extension| extension != "pub"))
        || name
            .rsplit_once('.')
            .is_some_and(|(_, extension)| EXTENSIONS.contains(&extension))
}

/// Why [`left_out`] leaves out a secret-named file.
pub const SECRET_FILE: &str = "it is a secret file";

/// Whether a file's name says it is a template of settings rather than the
/// settings (`.env.example`, `.env.sample`, `.env.template`, `.env.dist`).
#[must_use]
pub fn is_template(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy().to_ascii_lowercase();
        [".example", ".sample", ".template", ".dist"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
    })
}

/// Why Haarness leaves a file out of a prompt it builds by itself (an
/// instruction file, an `@` import in one, a file a skill or command names
/// with `@`), if it does: a secret-named file (`.env`), a file where
/// Haarness keeps sign-ins, or one `omit` refuses (a `read` rule that
/// denies it). `canonical` is where the file really is, and `workspace` the
/// project folder, also as it really is. Nothing asks before such a prompt
/// is sent, so the file is left out with a note instead.
#[must_use]
pub fn left_out(
    canonical: &Path,
    workspace: Option<&Path>,
    omit: ImportFilter<'_>,
) -> Option<&'static str> {
    let relative = workspace
        .and_then(|workspace| canonical.strip_prefix(workspace).ok())
        .unwrap_or(canonical);
    if crate::is_secret_path(relative) {
        return Some(SECRET_FILE);
    }
    // A read of one asks a person, and nobody is asked here.
    if crate::is_credential_store_path(canonical) {
        return Some("Haarness keeps sign-ins there");
    }
    omit(canonical)
}

/// What stays the same across one file's imports.
struct Scope<'a> {
    workspace: &'a Path,
    home: Option<&'a Path>,
    trusted: bool,
    omit: ImportFilter<'a>,
}

/// Why an import of `canonical` is left out, if it is.
fn omitted(canonical: &Path, workspace: Option<&Path>, scope: &Scope<'_>) -> Option<&'static str> {
    left_out(canonical, workspace, scope.omit)
}

fn resolve(
    path: &Path,
    scope: &Scope<'_>,
    budget: &mut ImportBudget,
    depth: usize,
) -> Option<String> {
    let (workspace, home, trusted) = (scope.workspace, scope.home, scope.trusted);
    if depth > MAX_DEPTH || budget.files == 0 {
        return None;
    }
    let content = read_within(path, MAX_FILE_BYTES.min(budget.bytes))?;
    // Text only: a file of NUL bytes (a device, `/proc/self/environ`) is no
    // instruction.
    if content.contains('\0') {
        return None;
    }
    budget.files -= 1;
    budget.bytes = budget.bytes.saturating_sub(content.len());
    let base = path.parent().unwrap_or(workspace);
    let references = references(&content);
    let mut next = references.iter().peekable();
    let mut out = String::with_capacity(content.len());
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push('\n');
        }
        while let Some(reference) = next.peek() {
            if reference.start >= offset {
                break;
            }
            let reference = next.next().expect("peeked");
            debug_assert!(reference.start >= line_start);
            // Bare tokens without a dot are mentions, emails or agent names
            // rather than files; quoted forms are always explicit.
            if !reference.quoted && !reference.path.contains('.') {
                continue;
            }
            let target = locate(&reference.path, base, home);
            let Ok(canonical) = crate::paths::canonicalize(&target) else {
                continue;
            };
            if !canonical.is_file() {
                continue;
            }
            let root = crate::paths::canonicalize(workspace).ok();
            let inside = root
                .as_deref()
                .is_some_and(|workspace| canonical.starts_with(workspace));
            if !inside && !trusted {
                continue; // outside the workspace: never auto-imported
            }
            if let Some(reason) = omitted(&canonical, root.as_deref(), scope) {
                write!(
                    out,
                    "\n[Instruction import omitted: {} ({reason})]\n",
                    reference.path
                )
                .expect("writing to a String cannot fail");
                continue;
            }
            if let Some(imported) = resolve(&canonical, scope, budget, depth + 1) {
                write!(
                    out,
                    "\n<imported from=\"{}\">\n{imported}\n</imported>\n",
                    reference.path
                )
                .expect("writing to a String cannot fail");
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_project_never_names_credentials_outside_itself_as_instructions() {
        let root = std::env::temp_dir().join(format!(
            "haarness-credential-instructions-{}",
            uuid::Uuid::now_v7()
        ));
        let project = root.join("project");
        let outside = root.join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let none: ImportFilter<'_> = &|_| None;
        for name in [
            "id_ed25519",
            "deploy.pem",
            ".netrc",
            "service.key",
            ".git-credentials",
        ] {
            let file = outside.join(name);
            std::fs::write(&file, "SECRET").unwrap();
            assert_eq!(
                file_left_out(&file, &project, false, none),
                Some(CREDENTIALS_FILE),
                "{name}"
            );
            // The person's own configuration may still name it.
            assert_eq!(file_left_out(&file, &project, true, none), None, "{name}");
        }
        for name in ["guide.md", "id_ed25519.pub"] {
            let file = outside.join(name);
            std::fs::write(&file, "fine").unwrap();
            assert_eq!(file_left_out(&file, &project, false, none), None, "{name}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("haarness-{name}-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn family_matches_first_member_per_directory_and_local_siblings() {
        let root = temp("family");
        fs::write(root.join("CLAUDE.md"), "claude").unwrap();
        fs::write(root.join("CLAUDE.local.md"), "local").unwrap();
        fs::write(root.join("CONTEXT.md"), "context").unwrap();
        assert!(find_in(&root, false).is_empty());
        assert_eq!(
            find_in(&root, true),
            vec![root.join("CLAUDE.md"), root.join("CLAUDE.local.md")]
        );
        fs::write(root.join("AGENTS.md"), "agents").unwrap();
        assert_eq!(find_in(&root, false), vec![root.join("AGENTS.md")]);
        assert_eq!(find_in(&root, true), vec![root.join("AGENTS.md")]);
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::remove_file(root.join("AGENTS.md")).unwrap();
        fs::remove_file(root.join("CLAUDE.md")).unwrap();
        fs::remove_file(root.join("CLAUDE.local.md")).unwrap();
        fs::write(root.join(".claude/CLAUDE.md"), "nested").unwrap();
        assert_eq!(find_in(&root, true), vec![root.join(".claude/CLAUDE.md")]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn attached_names_what_read_results_carried_also_inside_a_batch() {
        let result = |name: &str, metadata: serde_json::Value| haarness_protocol::Item {
            id: haarness_protocol::ItemId::new(),
            thread_id: haarness_protocol::ThreadId::new(),
            turn_id: None,
            sequence: 1,
            data: haarness_protocol::ItemData::ToolResult {
                call_id: "call".into(),
                name: name.into(),
                output: String::new(),
                is_error: false,
                metadata,
            },
            created_at: chrono::Utc::now(),
        };
        let items = [
            result("read", serde_json::json!({"loaded": ["/w/a/AGENTS.md"]})),
            // A symlink, named as read showed it and keyed by its target.
            result(
                "read",
                serde_json::json!({
                    "loaded": ["/w/link/AGENTS.md"],
                    "loadedResolved": ["/w/link/CLAUDE.md"],
                }),
            ),
            result(
                "batch",
                serde_json::json!({"results": [
                    {"tool": "read", "metadata": {"loaded": ["/w/b/AGENTS.md"]}},
                    {"tool": "read", "error": "denied"},
                ]}),
            ),
            result("bash", serde_json::json!({"exit": 0})),
            result("read", serde_json::Value::Null),
        ];
        let found = attached(&items);
        assert_eq!(
            found,
            [
                PathBuf::from("/w/a/AGENTS.md"),
                PathBuf::from("/w/link/CLAUDE.md"),
                PathBuf::from("/w/b/AGENTS.md")
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn nested_discovery_walks_up_to_but_not_including_the_workspace() {
        let root = crate::paths::canonicalize(temp("nested")).unwrap();
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::write(root.join("AGENTS.md"), "root").unwrap();
        fs::write(root.join("a/AGENTS.md"), "a").unwrap();
        fs::write(root.join("a/b/CLAUDE.md"), "b").unwrap();
        fs::write(root.join("a/b/c/file.rs"), "code").unwrap();
        assert_eq!(
            nested(&root.join("a/b/c/file.rs"), &root, false),
            vec![root.join("a/AGENTS.md")]
        );
        assert_eq!(
            nested(&root.join("a/b/c/file.rs"), &root, true),
            vec![root.join("a/b/CLAUDE.md"), root.join("a/AGENTS.md")]
        );
        assert!(nested(&root.join("AGENTS.md"), &root, true).is_empty());
        assert!(nested(&std::env::temp_dir().join("elsewhere.rs"), &root, true).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_files_are_judged_where_they_really_are_and_by_their_size() {
        let root = crate::paths::canonicalize(temp("nested-judged")).unwrap();
        // A repository opened at a subfolder: its worktree root is the
        // project too.
        let repository = root.join("repo");
        let workspace = repository.join("app");
        for folder in [
            repository.join(".git"),
            repository.join("shared"),
            workspace.join("sub"),
            workspace.join("private"),
            root.join("notes"),
            root.join("home/.claude"),
            root.join("home/.config/gh"),
        ] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(repository.join("shared/rules.md"), "SHARED").unwrap();
        fs::write(root.join("notes/rules.md"), "NOTES").unwrap();
        fs::write(root.join("notes/history"), "HISTORY").unwrap();
        fs::write(root.join(".zsh_history"), "HISTORY").unwrap();
        fs::write(root.join("home/.claude/CLAUDE.md"), "PERSONAL").unwrap();
        fs::write(root.join("home/.config/gh/hosts.md"), "TOKEN").unwrap();
        fs::write(root.join("notes/id_rules.md"), "KEY").unwrap();
        fs::write(workspace.join(".env"), "ENV_SECRET").unwrap();
        fs::write(workspace.join("private/rules.md"), "PRIVATE").unwrap();
        let plain = workspace.join("sub/AGENTS.md");
        fs::write(&plain, "PLAIN").unwrap();
        let personal = personal_folders(None, Some(&root.join("home")));
        let none: ImportFilter<'_> = &|_| None;
        assert_eq!(nested_left_out(&plain, &workspace, &personal, none), None);
        // Over 40 KiB, measured: a sparse 2 GiB file is never read.
        let large = workspace.join("sub/CLAUDE.md");
        fs::write(&large, "x".repeat(MAX_FILE_BYTES + 1)).unwrap();
        assert_eq!(
            nested_left_out(&large, &workspace, &personal, none),
            Some(TOO_LARGE)
        );
        fs::File::create(&large).unwrap().set_len(2 << 30).unwrap();
        assert_eq!(
            nested_left_out(&large, &workspace, &personal, none),
            Some(TOO_LARGE)
        );
        assert_eq!(read_small(&large), None);
        // A read rule, on the file as named or where a link leads it.
        let private = workspace.join("private");
        let deny = |file: &Path| {
            (file.starts_with(&private) || file == plain).then_some("a read rule denies it")
        };
        assert_eq!(
            nested_left_out(&plain, &workspace, &personal, &deny),
            Some("a read rule denies it")
        );
        #[cfg(unix)]
        for (target, reason) in [
            // Inside the repository, beside the opened folder.
            (repository.join("shared/rules.md"), None),
            // A personal note, also in the person's own instruction folder.
            (root.join("notes/rules.md"), None),
            (root.join("home/.claude/CLAUDE.md"), None),
            (root.join(".zsh_history"), Some(LINKED_OUT)),
            (root.join("notes/history"), Some(LINKED_OUT)),
            (root.join("home/.config/gh/hosts.md"), Some(LINKED_OUT)),
            (root.join("notes/id_rules.md"), Some(CREDENTIALS_FILE)),
            (workspace.join(".env"), Some(SECRET_FILE)),
            (
                workspace.join("private/rules.md"),
                Some("a read rule denies it"),
            ),
        ] {
            let link = workspace.join("sub/AGENTS.md");
            fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let deny = |file: &Path| {
                file.starts_with(&private)
                    .then_some("a read rule denies it")
            };
            assert_eq!(
                nested_left_out(&link, &workspace, &personal, &deny),
                reason,
                "{}",
                target.display()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_link_out_of_the_project_loads_only_a_plain_markdown_note() {
        let roots = [PathBuf::from("/work/repo")];
        let personal = [
            PathBuf::from("/home/me/.claude"),
            PathBuf::from("/home/me/.config/haarness"),
        ];
        for (file, reason) in [
            ("/work/repo/.env", None),
            ("/work/repo/docs/x.txt", None),
            ("/home/me/notes/rules.md", None),
            ("/home/me/notes/Rules.MDX", None),
            ("/home/me/notes/rules.markdown", None),
            ("/home/me/.claude/CLAUDE.md", None),
            ("/home/me/.config/haarness/AGENTS.md", None),
            ("/home/me/.zsh_history", Some(LINKED_OUT)),
            ("/proc/self/environ", Some(LINKED_OUT)),
            ("/dev/zero", Some(LINKED_OUT)),
            ("/home/me/notes/.hidden.md", Some(LINKED_OUT)),
            ("/home/me/notes/codes.txt", Some(LINKED_OUT)),
            ("/home/me/.config/gh/hosts.md", Some(LINKED_OUT)),
            ("/home/me/.claude/.cache/x.md", Some(LINKED_OUT)),
        ] {
            assert_eq!(
                linked_out(Path::new(file), &roots, &personal),
                reason,
                "{file}"
            );
        }
    }

    #[test]
    fn the_size_reason_comes_from_the_file_size_and_the_room_left() {
        let root = temp("sizes");
        let small = root.join("small.md");
        fs::write(&small, "x".repeat(1024)).unwrap();
        let large = root.join("large.md");
        fs::File::create(&large).unwrap().set_len(2 << 30).unwrap();
        let budget = ImportBudget::default();
        assert_eq!(size_left_out(&small, &budget), None);
        assert_eq!(size_left_out(&large, &budget), Some(TOO_LARGE));
        assert_eq!(size_left_out(&root.join("missing.md"), &budget), None);
        assert_eq!(
            size_left_out(&root, &budget),
            None,
            "a folder is skipped quietly"
        );
        let spent = ImportBudget {
            files: 3,
            bytes: 512,
        };
        assert_eq!(size_left_out(&small, &spent), Some(NO_ROOM));
        let counted = ImportBudget {
            files: 0,
            bytes: 200 * 1024,
        };
        assert_eq!(size_left_out(&small, &counted), Some(NO_ROOM));
        // The assembly leaves the large file out without reading it.
        let mut budget = ImportBudget::default();
        assert_eq!(
            read_with_imports(&large, &root, &mut budget, true, &|_| None),
            None
        );
        assert_eq!(budget.files, ImportBudget::default().files);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn references_support_quoted_forms_and_skip_code() {
        let text = "See @docs/a.md and @\"my docs/b.md\" then @'c d.md' or @<e f.md>.\n\
                    Email me@example.com, not `@literal.md` nor ``@x.md``.\n\
                    ```\n@fenced.md\n```\n@tail.md.";
        let found = references(text);
        let paths: Vec<&str> = found.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["docs/a.md", "my docs/b.md", "c d.md", "e f.md", "tail.md"]
        );
        assert!(found[1].quoted && !found[0].quoted);
        for reference in &found {
            let raw = &text[reference.start..reference.end];
            assert!(raw.starts_with('@'), "{raw}");
        }
        assert_eq!(&text[found[1].start..found[1].end], "@\"my docs/b.md\"");
        assert_eq!(mask_code("a `b` c").len(), "a `b` c".len());
        // Masking is byte-preserving: the two-byte `ü` becomes two spaces.
        assert_eq!(mask_code("é `ü` x"), "é      x");
    }

    #[test]
    fn imports_follow_quoted_paths_and_honour_budgets() {
        let root = temp("imports");
        fs::create_dir_all(root.join("with space")).unwrap();
        fs::write(root.join("with space/extra.md"), "EXTRA").unwrap();
        fs::write(root.join("plain.md"), "PLAIN @missing.md").unwrap();
        fs::write(
            root.join("AGENTS.md"),
            "rule\n@\"with space/extra.md\" and @plain.md\n`@plain.md`\n@self.md",
        )
        .unwrap();
        let mut budget = ImportBudget::default();
        let text = read_with_imports(&root.join("AGENTS.md"), &root, &mut budget, false, &|_| {
            None
        })
        .unwrap();
        assert_eq!(text.matches("EXTRA").count(), 1);
        assert_eq!(text.matches("PLAIN").count(), 1);
        assert!(text.contains("<imported from=\"with space/extra.md\">"));
        assert_eq!(budget.files, 20 - 3);

        let mut exhausted = ImportBudget {
            files: 1,
            bytes: 1024,
        };
        let text = read_with_imports(
            &root.join("AGENTS.md"),
            &root,
            &mut exhausted,
            false,
            &|_| None,
        )
        .unwrap();
        assert!(!text.contains("EXTRA"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn secret_files_and_files_a_rule_refuses_are_never_imported() {
        let root = temp("omitted");
        fs::create_dir_all(root.join("private")).unwrap();
        fs::create_dir_all(root.join("public")).unwrap();
        fs::write(root.join(".env"), "ENV_SECRET").unwrap();
        fs::write(root.join(".env.example"), "ENV_EXAMPLE").unwrap();
        fs::write(root.join("private/notes.md"), "PRIVATE_BODY").unwrap();
        fs::write(root.join("public/notes.md"), "PUBLIC_BODY").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../private/notes.md", root.join("public/link.md")).unwrap();
        fs::write(
            root.join("AGENTS.md"),
            "@.env\n@.env.example\n@private/notes.md\n@public/notes.md\n@public/link.md\n",
        )
        .unwrap();
        let private = crate::paths::canonicalize(&root).unwrap().join("private");
        let deny = |file: &Path| {
            file.starts_with(&private)
                .then_some("a read rule denies it")
        };
        let mut budget = ImportBudget::default();
        let text =
            read_with_imports(&root.join("AGENTS.md"), &root, &mut budget, false, &deny).unwrap();
        for hidden in ["ENV_SECRET", "PRIVATE_BODY"] {
            assert!(!text.contains(hidden), "{text}");
        }
        for shown in ["ENV_EXAMPLE", "PUBLIC_BODY"] {
            assert!(text.contains(shown), "{text}");
        }
        assert!(text.contains("[Instruction import omitted: .env (it is a secret file)]"));
        assert!(
            text.contains("[Instruction import omitted: private/notes.md (a read rule denies it)]")
        );
        #[cfg(unix)]
        assert!(
            text.contains("[Instruction import omitted: public/link.md (a read rule denies it)]"),
            "{text}"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
