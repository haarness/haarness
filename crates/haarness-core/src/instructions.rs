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

/// Largest instruction file that is inlined; bigger files are skipped.
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
    let root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let target = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
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
/// name an existing file are left as written. A secret-named file (`.env`),
/// or one `omit` refuses, is never read: an `[Instruction import omitted: …]`
/// line takes its place, as nothing asks before instructions load.
#[must_use]
pub fn read_with_imports(
    path: &Path,
    workspace: &Path,
    budget: &mut ImportBudget,
    trusted: bool,
    omit: ImportFilter<'_>,
) -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let scope = Scope {
        workspace,
        home: home.as_deref(),
        trusted,
        omit,
    };
    resolve(path, &scope, budget, 0)
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
    let relative = workspace
        .and_then(|workspace| canonical.strip_prefix(workspace).ok())
        .unwrap_or(canonical);
    if crate::is_secret_path(relative) {
        return Some("it is a secret file");
    }
    (scope.omit)(canonical)
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
    let content = fs::read_to_string(path).ok()?;
    if content.len() > MAX_FILE_BYTES || content.len() > budget.bytes {
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
            let Ok(canonical) = target.canonicalize() else {
                continue;
            };
            if !canonical.is_file() {
                continue;
            }
            let root = workspace.canonicalize().ok();
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
    fn nested_discovery_walks_up_to_but_not_including_the_workspace() {
        let root = temp("nested").canonicalize().unwrap();
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
        let private = root.canonicalize().unwrap().join("private");
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
