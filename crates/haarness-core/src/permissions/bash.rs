//! Shell-aware permission analysis for bash commands.
//! The command is parsed with tree-sitter-bash, every
//! `command` node yields an exact `bash:` operation plus a generalized
//! "always" prefix pattern, and path-taking commands / redirect targets that
//! escape the workspace raise `external_directory` operations.

use std::path::{Path, PathBuf};

use super::arity;

/// What one bash invocation needs permission-wise.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BashAnalysis {
    /// Exact per-command patterns, e.g. `git status --short`.
    pub patterns: Vec<String>,
    /// Generalized "don't ask again" patterns, e.g. `git status *`.
    pub always: Vec<String>,
    /// Directory globs outside the workspace this command touches,
    /// e.g. `/tmp/*`.
    pub external_dirs: Vec<String>,
}

/// Commands whose non-flag arguments are filesystem paths worth resolving.
const PATH_COMMANDS: &[&str] = &[
    "cd", "rm", "cp", "mv", "mkdir", "touch", "chmod", "chown", "cat",
];

/// Analyze `command` as it would run from `workspace`. Parse failures fall
/// back to treating the whole command as one opaque pattern (never silently
/// allowing).
#[must_use]
pub fn analyze(command: &str, workspace: &Path) -> BashAnalysis {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_bash::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return fallback(command);
    }
    let Some(tree) = parser.parse(command, None) else {
        return fallback(command);
    };
    let mut analysis = BashAnalysis::default();
    let mut stack = vec![tree.root_node()];
    let mut command_nodes = Vec::new();
    let mut redirects = Vec::new();
    while let Some(node) = stack.pop() {
        if node.kind() == "command" {
            command_nodes.push(node);
        }
        if node.kind() == "file_redirect" {
            redirects.push(node);
        }
        for index in (0..node.child_count()).rev() {
            if let Some(child) = node.child(index) {
                stack.push(child);
            }
        }
    }
    // The traversal above visits parents before children but pops LIFO;
    // sort by position so patterns keep source order.
    command_nodes.sort_by_key(tree_sitter::Node::start_byte);

    for node in command_nodes {
        // Redirects belong to the command they feed.
        let command_text = match node.parent() {
            Some(parent) if parent.kind() == "redirected_statement" => node_text(parent, command),
            _ => node_text(node, command),
        };
        let mut tokens = Vec::new();
        for index in 0..node.child_count() {
            let Some(child) = node.child(index) else {
                continue;
            };
            if matches!(
                child.kind(),
                "command_name" | "word" | "number" | "string" | "raw_string" | "concatenation"
            ) {
                tokens.push(node_text(child, command));
            }
        }
        let paths = (0..node.named_child_count())
            .filter_map(|index| node.named_child(index))
            .filter(|child| {
                matches!(
                    child.kind(),
                    "command_name" | "word" | "number" | "string" | "raw_string" | "concatenation"
                )
            })
            .map(|child| literal(child, command))
            .collect::<Vec<_>>();
        if let Some((first, true)) = paths.first() {
            if PATH_COMMANDS.contains(&first.as_str()) {
                for (argument, complete) in &paths[1..] {
                    if !complete
                        || argument.starts_with('-')
                        || (first == "chmod" && argument.starts_with('+'))
                    {
                        continue;
                    }
                    if let Some(dir) = external_dir(workspace, argument, true) {
                        push_unique(&mut analysis.external_dirs, glob(&dir));
                    }
                }
            }
        }
        if let Some(first) = tokens.first() {
            if first != "cd" {
                push_unique(&mut analysis.patterns, command_text);
                let prefix = arity::prefix(&tokens).join(" ");
                push_unique(&mut analysis.always, format!("{prefix} *"));
            }
        }
    }
    for redirect in redirects {
        let Some(dest) = redirect.child_by_field_name("destination") else {
            continue;
        };
        let (value, complete) = literal(dest, command);
        let descriptor = (0..redirect.child_count())
            .filter_map(|i| redirect.child(i))
            .any(|child| matches!(node_text(child, command).as_str(), ">&" | "<&"));
        if descriptor
            && complete
            && (value == "-"
                || value
                    .trim_end_matches('-')
                    .chars()
                    .all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        let target = if complete {
            value.as_str()
        } else {
            value.rfind('/').map_or("", |end| &value[..=end])
        };
        if target.is_empty() || target.starts_with("/dev/") {
            continue;
        }
        if let Some(dir) = external_dir(workspace, target, false) {
            let dir = if complete {
                dir
            } else {
                resolve(workspace, target)
            };
            push_unique(&mut analysis.external_dirs, glob(&dir));
        }
    }
    if analysis.patterns.is_empty() && analysis.external_dirs.is_empty() {
        return fallback(command);
    }
    analysis
}

/// Whether a shell program is structurally limited to a conservative set of
/// inspection commands. This is intentionally an allowlist: an unknown
/// executable, output redirection, parse error, or command-specific execution
/// escape keeps the command in the mutation/snapshot lane.
#[must_use]
pub fn is_obviously_read_only(command: &str) -> bool {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_bash::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return false;
    }
    let Some(tree) = parser.parse(command, None) else {
        return false;
    };
    if tree.root_node().has_error() {
        return false;
    }

    let mut stack = vec![tree.root_node()];
    let mut saw_command = false;
    while let Some(node) = stack.pop() {
        // Even a read-only command can write through `>`/`>>`. Staying
        // conservative for input and descriptor redirects keeps this check
        // independent of shell-specific redirect semantics.
        if node.kind() == "redirected_statement" {
            return false;
        }
        if node.kind() == "command" {
            saw_command = true;
            let mut tokens = Vec::new();
            for index in 0..node.child_count() {
                let Some(child) = node.child(index) else {
                    continue;
                };
                if matches!(child.kind(), "variable_assignment" | "comment") {
                    continue;
                }
                let (value, complete) = literal(child, command);
                if !complete {
                    return false;
                }
                tokens.push(value);
            }
            if !read_only_invocation(&tokens) {
                return false;
            }
        }
        for index in 0..node.child_count() {
            if let Some(child) = node.child(index) {
                stack.push(child);
            }
        }
    }
    saw_command || command.trim().is_empty()
}

fn read_only_invocation(tokens: &[String]) -> bool {
    let Some(program) = tokens.first().map(String::as_str) else {
        return false;
    };
    // Explicit paths and dynamically selected executables are opaque even if
    // their basename resembles an allowlisted utility.
    if program.contains(['/', '\\']) || program.contains(['$', '`']) {
        return false;
    }
    let args = &tokens[1..];
    match program {
        ":" | "true" | "false" | "pwd" | "printf" | "echo" | "cat" | "head" | "tail" | "wc"
        | "cut" | "tr" | "uniq" | "grep" | "ls" | "stat" | "file" | "du" | "df" | "basename"
        | "dirname" | "realpath" | "cmp" | "comm" | "md5" | "md5sum" | "sha1sum" | "sha256sum"
        | "shasum" | "uname" | "date" | "which" | "type" | "test" | "[" | "jq" | "cd" => true,
        "sleep" => {
            args.len() == 1
                && !args[0].is_empty()
                && args[0].bytes().all(|byte| {
                    byte.is_ascii_digit() || matches!(byte, b'.' | b's' | b'm' | b'h' | b'd')
                })
        }
        "sort" => !args.iter().any(|arg| {
            arg == "-o"
                || (arg.starts_with("-o") && arg.len() > 2)
                || arg == "--output"
                || arg.starts_with("--output=")
                || arg == "--compress-program"
                || arg.starts_with("--compress-program=")
        }),
        "rg" => !args
            .iter()
            .any(|arg| arg == "--pre" || arg.starts_with("--pre=")),
        "find" => !args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "-delete"
                    | "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
                    | "-fls"
            )
        }),
        "fd" => !args.iter().any(|arg| {
            matches!(arg.as_str(), "-x" | "--exec" | "-X" | "--exec-batch")
                || arg.starts_with("--exec=")
                || arg.starts_with("--exec-batch=")
        }),
        "git" => {
            args.first().is_some_and(|subcommand| {
                matches!(
                    subcommand.as_str(),
                    "status"
                        | "diff"
                        | "log"
                        | "show"
                        | "ls-files"
                        | "ls-tree"
                        | "rev-parse"
                        | "cat-file"
                        | "grep"
                        | "describe"
                        | "shortlog"
                        | "blame"
                        | "name-rev"
                        | "merge-base"
                        | "for-each-ref"
                        | "count-objects"
                        | "version"
                )
            }) && !args.iter().skip(1).any(|arg| {
                arg == "--output"
                    || arg.starts_with("--output=")
                    || arg == "--ext-diff"
                    || arg == "--textconv"
            })
        }
        _ => false,
    }
}

fn fallback(command: &str) -> BashAnalysis {
    let tokens: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
    let always = if tokens.is_empty() {
        Vec::new()
    } else {
        vec![format!("{} *", arity::prefix(&tokens).join(" "))]
    };
    BashAnalysis {
        patterns: vec![command.to_owned()],
        always,
        external_dirs: Vec::new(),
    }
}

fn node_text(node: tree_sitter::Node, source: &str) -> String {
    source
        .get(node.start_byte()..node.end_byte())
        .unwrap_or_default()
        .to_owned()
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn glob(dir: &Path) -> String {
    let text = dir.display().to_string();
    format!("{}/*", text.trim_end_matches('/'))
}

/// The directory a path argument points at when it escapes the workspace.
/// `must_exist` controls discovery: path-command arguments are only flagged
/// when they resolve (realpath), redirect targets are flagged even when the
/// file does not exist yet.
fn external_dir(workspace: &Path, argument: &str, must_exist: bool) -> Option<PathBuf> {
    if must_exist && !workspace.join(argument).exists() {
        return None;
    }
    let resolved = resolve(workspace, argument);
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    if resolved.starts_with(&workspace) {
        return None;
    }
    if resolved.is_dir() {
        Some(resolved)
    } else {
        Some(resolved.parent()?.to_path_buf())
    }
}

/// Resolve nonexistent leaves through the nearest existing ancestor too.
fn resolve(workspace: &Path, argument: &str) -> PathBuf {
    let joined = workspace.join(argument);
    let mut ancestor = joined.as_path();
    let mut suffix = Vec::new();
    loop {
        if let Ok(mut resolved) = ancestor.canonicalize() {
            for part in suffix.into_iter().rev() {
                if part == ".." {
                    resolved.pop();
                } else if part != "." {
                    resolved.push(part);
                }
            }
            return resolved;
        }
        let Some(name) = ancestor.file_name() else {
            return joined;
        };
        suffix.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return joined;
        };
        ancestor = parent;
    }
}

/// Decode literal shell syntax without evaluating variables or substitutions.
/// Incomplete values retain only their known leading portion for path checks.
fn literal(node: tree_sitter::Node<'_>, source: &str) -> (String, bool) {
    let text = node_text(node, source);
    match node.kind() {
        "raw_string" => (text[1..text.len() - 1].into(), true),
        "command_name" | "concatenation" | "string" => {
            let mut value = String::new();
            for index in 0..node.named_child_count() {
                let Some(child) = node.named_child(index) else {
                    continue;
                };
                let (part, complete) = literal(child, source);
                value.push_str(&part);
                if !complete {
                    return (value, false);
                }
            }
            (value, true)
        }
        "word" | "number" | "string_content" => {
            let mut value = String::new();
            let mut chars = text.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        if next == '\n' {
                            continue;
                        }
                        if node.kind() == "string_content"
                            && !matches!(next, '\\' | '"' | '$' | '`')
                        {
                            value.push(c);
                        }
                        value.push(next);
                    }
                } else {
                    value.push(c);
                }
            }
            (value, true)
        }
        _ => (String::new(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> PathBuf {
        std::env::current_dir().expect("cwd")
    }

    #[test]
    fn single_command_yields_exact_pattern_and_arity_always() {
        let analysis = analyze("git status --short", &workspace());
        assert_eq!(analysis.patterns, vec!["git status --short"]);
        assert_eq!(analysis.always, vec!["git status *"]);
        assert!(analysis.external_dirs.is_empty());
    }

    #[test]
    fn command_chains_yield_one_pattern_per_command_node() {
        let analysis = analyze(
            "git add -A && git commit -m 'x' || echo failed",
            &workspace(),
        );
        assert_eq!(
            analysis.patterns,
            vec!["git add -A", "git commit -m 'x'", "echo failed"]
        );
        assert_eq!(analysis.always, vec!["git add *", "git commit *", "echo *"]);
    }

    #[test]
    fn pipes_and_substitution_are_commands_too() {
        let analysis = analyze("cat Cargo.toml | grep name", &workspace());
        assert_eq!(analysis.patterns, vec!["cat Cargo.toml", "grep name"]);
    }

    #[test]
    fn redirect_outside_the_workspace_flags_the_directory() {
        let analysis = analyze("echo hi > /tmp/haarness-test-out.txt", &workspace());
        assert!(
            analysis
                .external_dirs
                .iter()
                .any(|dir| dir.ends_with("/*") && dir.contains("tmp")),
            "{:?}",
            analysis.external_dirs
        );
        // The pattern keeps the full redirected statement.
        assert_eq!(
            analysis.patterns,
            vec!["echo hi > /tmp/haarness-test-out.txt"]
        );
    }

    #[test]
    fn descriptor_redirects_and_dev_null_stay_quiet() {
        let analysis = analyze("cargo build 2>&1 > /dev/null", &workspace());
        assert!(analysis.external_dirs.is_empty(), "{analysis:?}");
    }

    #[test]
    fn path_commands_flag_external_arguments() {
        let analysis = analyze("cat /etc/hosts", &workspace());
        // macOS canonicalizes /etc to /private/etc.
        assert!(
            analysis.external_dirs[0].ends_with("etc/*"),
            "{:?}",
            analysis.external_dirs
        );
        // Workspace-relative paths stay quiet.
        let local = analyze("cat Cargo.toml", &workspace());
        assert!(local.external_dirs.is_empty());
    }

    #[test]
    fn cd_contributes_directory_checks_but_no_command_pattern() {
        let analysis = analyze("cd /tmp", &workspace());
        assert!(analysis.patterns.is_empty(), "{analysis:?}");
        assert_eq!(analysis.external_dirs, vec!["/private/tmp/*"]);
    }

    #[test]
    fn unparseable_input_falls_back_to_an_opaque_pattern() {
        let analysis = analyze("plain text prompt", &workspace());
        assert_eq!(analysis.patterns, vec!["plain text prompt"]);
        assert_eq!(analysis.always, vec!["plain *"]);
    }

    #[test]
    fn obvious_inspection_pipeline_is_read_only() {
        assert!(is_obviously_read_only(
            "find crates -maxdepth 3 -type f | sort && printf '\\n--- workflows ---\\n' && find .github scripts xtask tests -maxdepth 4 -type f | sort"
        ));
        assert!(is_obviously_read_only(
            "git status --short && rg -n 'unsafe|token' crates"
        ));
        assert!(is_obviously_read_only("sleep 1"));
    }

    #[test]
    fn shell_escape_hatches_remain_in_the_mutation_lane() {
        for command in [
            "find . -delete",
            "find . -exec touch changed {} +",
            "sort input -o output",
            "sort input -ooutput",
            "sort input --output=output",
            "sort --compress-program='touch changed' input",
            "rg --pre 'touch changed' needle",
            "git diff --output=changed",
            "git diff --ext-diff",
            "echo changed > file",
            "./looks-safe status",
            "cargo test",
        ] {
            assert!(!is_obviously_read_only(command), "{command}");
        }
    }
}

#[cfg(test)]
mod regression {
    use super::*;

    #[test]
    fn redirects_use_shell_destinations_including_quotes_and_read_redirects() {
        let root = std::env::temp_dir().join(format!("haarness-redirect-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("project");
        let outside = root.join("external space");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let expected = vec![glob(&outside.canonicalize().unwrap())];
        std::fs::write(outside.join("input"), "probe").unwrap();
        assert_eq!(
            analyze(&format!("'cat' '{}/'input", outside.display()), &workspace).external_dirs,
            expected
        );

        for operator in [">", ">>", "<", "<>", ">|", "2>", "&>", "&>>"] {
            for target in [
                format!("'{}/out'", outside.display()),
                format!("\"{}/out\"", outside.display()),
                format!("'{}/'$name", outside.display()),
                format!("'{}/'$(printf out)", outside.display()),
            ] {
                let command = format!("printf probe {operator} {target}");
                assert_eq!(
                    analyze(&command, &workspace).external_dirs,
                    expected,
                    "{command}"
                );
            }
        }
        assert_eq!(
            analyze(&format!("> '{}/out'", outside.display()), &workspace).external_dirs,
            expected
        );
        assert!(
            analyze("echo 'text > /tmp/example'", &workspace)
                .external_dirs
                .is_empty()
        );
        assert!(analyze("echo x 2>&1", &workspace).external_dirs.is_empty());
        assert!(
            analyze("cat <<EOF\n> /tmp/example\nEOF", &workspace)
                .external_dirs
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quoted_and_dynamic_options_cannot_hide_shell_writes() {
        for command in [
            "sort input '-o' output",
            "sort input \"-o\" output",
            "sort input -'o' output",
            "find . '-delete'",
            "git diff '--output=changed'",
            "rg '--pre=touch changed' needle",
            "sort input $option output",
        ] {
            assert!(!is_obviously_read_only(command), "{command}");
        }
        for command in [
            "git status '--short'",
            "printf '%s' hello",
            "find . -name '*.rs'",
            "cat 'file with spaces'",
        ] {
            assert!(is_obviously_read_only(command), "{command}");
        }
    }
}
