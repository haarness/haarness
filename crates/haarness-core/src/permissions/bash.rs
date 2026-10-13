//! Shell-aware permission analysis for bash commands.
//! The command is parsed with tree-sitter-bash, every
//! `command` node yields an exact `bash:` operation plus a generalized
//! "always" prefix pattern, and path-taking commands / redirect targets that
//! escape the workspace raise `external_directory` operations.

use std::path::{Component, Path, PathBuf};

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
            .map(|child| {
                let (value, complete) = literal(child, command);
                (value, complete, node_text(child, command))
            })
            .collect::<Vec<_>>();
        if let Some((first, true, _)) = paths.first() {
            if PATH_COMMANDS.contains(&first.as_str()) {
                for (argument, complete, raw) in &paths[1..] {
                    if argument.starts_with('-') || (first == "chmod" && argument.starts_with('+'))
                    {
                        continue;
                    }
                    if let Some(dir) = external_dir(workspace, raw, argument, *complete) {
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
        if value.starts_with("/dev/") {
            continue;
        }
        if let Some(dir) = external_dir(workspace, &node_text(dest, command), &value, complete) {
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

/// The directory a path argument or redirect target points at when it
/// escapes the workspace. A path that does not exist yet (a `touch`, `mkdir`
/// or `cp` target, a new redirect file) is judged where it would be created,
/// so a new file outside the project asks like an existing one. A value with
/// a dynamic part (`/tmp/$name`) is judged by the folder its literal start
/// names. `raw` is the source text, which tells an unquoted leading `~`.
fn external_dir(workspace: &Path, raw: &str, value: &str, complete: bool) -> Option<PathBuf> {
    let target = if complete {
        value
    } else {
        value.rfind('/').map_or("", |end| &value[..=end])
    };
    if target.is_empty() {
        return None;
    }
    let resolved = match home_expansion(raw, target) {
        Some(expanded) => resolve(workspace, &expanded),
        None => resolve(workspace, Path::new(target)),
    };
    let workspace = resolve(workspace, Path::new(""));
    if resolved.starts_with(&workspace) {
        return None;
    }
    if !complete || resolved.is_dir() {
        Some(resolved)
    } else {
        Some(resolved.parent().unwrap_or(&resolved).to_path_buf())
    }
}

/// The folder an unquoted leading `~` expands to: `~` and `~/…` are the
/// user's home, `~name/…` that user's home, taken as a sibling of this one.
/// Without a known home the path is put at `/`, which is never the project.
fn home_expansion(raw: &str, value: &str) -> Option<PathBuf> {
    if !raw.starts_with('~') || !value.starts_with('~') {
        return None;
    }
    let (user, rest) = value[1..].split_once('/').unwrap_or((&value[1..], ""));
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/"));
    let base = if user.is_empty() {
        home
    } else {
        home.parent().unwrap_or(Path::new("/")).join(user)
    };
    Some(base.join(rest))
}

/// How many links one path may pass through before the check gives up and
/// treats it as outside the project (the system's own limit is similar).
const MAX_LINKS: u8 = 40;

/// Where `argument` lands when a command runs from `workspace`, as the
/// system resolves it: links (dangling ones too) and the `..` after them
/// are followed through the part that exists, and the part that does not
/// exist yet is laid onto its nearest existing folder, with `..` taken
/// lexically there (what `mkdir -p` creates). Unresolvable link chains
/// resolve to `/`, outside every project.
fn resolve(workspace: &Path, argument: impl AsRef<Path>) -> PathBuf {
    resolve_path(&workspace.join(argument), 0)
}

fn resolve_path(path: &Path, depth: u8) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    let mut resolved = PathBuf::new();
    // Once a component is missing, the rest is laid on lexically.
    let mut missing = false;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            // `resolved` holds no links up to its first missing component,
            // so its parent is where the system's `..` goes. Climbing back
            // out of the missing part resumes following links.
            Component::ParentDir => {
                resolved.pop();
                missing = missing && std::fs::symlink_metadata(&resolved).is_err();
            }
            Component::Normal(name) => {
                resolved.push(name);
                if missing {
                    continue;
                }
                match std::fs::symlink_metadata(&resolved) {
                    Err(_) => missing = true,
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        let Ok(target) = std::fs::read_link(&resolved) else {
                            return PathBuf::from("/");
                        };
                        if depth >= MAX_LINKS {
                            return PathBuf::from("/");
                        }
                        resolved.pop();
                        resolved = resolve_path(&resolved.join(target), depth + 1);
                        missing = std::fs::symlink_metadata(&resolved).is_err();
                    }
                    Ok(_) => {}
                }
            }
        }
    }
    resolved
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
    fn new_paths_outside_the_project_ask_like_existing_ones() {
        let root =
            std::env::temp_dir().join(format!("haarness-new-paths-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("project");
        let outside = root.join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(workspace.join("file"), "x").unwrap();
        let outside_dir = outside.canonicalize().unwrap();
        let expected = vec![glob(&outside_dir)];
        let new = outside.join("new");
        for command in [
            format!("touch {}", new.display()),
            format!("mkdir {}", new.display()),
            format!("cp file {}", new.display()),
            format!("mv file {}", new.display()),
            format!("chmod 600 {}", new.display()),
            format!("echo x > {}", new.display()),
            format!("echo x >> '{}'", new.display()),
            format!("touch {}/$name", outside.display()),
            format!("rm -rf {}/", outside.display()),
        ] {
            assert_eq!(
                analyze(&command, &workspace).external_dirs,
                expected,
                "{command}"
            );
        }
        // A chain `mkdir -p` creates is judged where it ends, `..` included.
        assert_eq!(
            analyze(&format!("mkdir -p {}/a/b", new.display()), &workspace).external_dirs,
            vec![glob(&outside_dir.join("new/a"))]
        );
        assert_eq!(
            analyze("mkdir -p fresh/../../outside/made", &workspace).external_dirs,
            expected
        );
        // New paths inside the project stay quiet.
        for command in [
            "touch fresh",
            "mkdir -p a/b/c",
            "cp file copy",
            "echo x > out.txt",
        ] {
            assert!(
                analyze(command, &workspace).external_dirs.is_empty(),
                "{command}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_lead_new_paths_outside_the_project() {
        let root =
            std::env::temp_dir().join(format!("haarness-new-links-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("project");
        let outside = root.join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        let outside_dir = outside.canonicalize().unwrap();
        // A dangling link: writing through it creates its target.
        std::os::unix::fs::symlink(outside.join("missing"), workspace.join("dangling")).unwrap();
        // `..` after a link goes up from where the link leads.
        std::os::unix::fs::symlink(outside.join("sub"), workspace.join("linked")).unwrap();
        // A link chain that never ends is outside every project.
        std::os::unix::fs::symlink("loop-b", workspace.join("loop-a")).unwrap();
        std::os::unix::fs::symlink("loop-a", workspace.join("loop-b")).unwrap();
        for (command, expected) in [
            ("touch dangling", glob(&outside_dir)),
            ("echo x > dangling", glob(&outside_dir)),
            ("touch linked/new", glob(&outside_dir.join("sub"))),
            ("touch linked/../new", glob(&outside_dir)),
            (
                "mkdir -p linked/../made/deeper",
                glob(&outside_dir.join("made")),
            ),
            (
                "mkdir -p fresh/../linked/made",
                glob(&outside_dir.join("sub")),
            ),
            ("touch loop-a/new", "/*".to_owned()),
        ] {
            assert_eq!(
                analyze(command, &workspace).external_dirs,
                vec![expected],
                "{command}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unquoted_tilde_is_the_home_folder() {
        let root = std::env::temp_dir().join(format!("haarness-tilde-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .unwrap_or_else(|| PathBuf::from("/"));
        let home = resolve(&home, "");
        if workspace.canonicalize().unwrap().starts_with(&home) {
            // A temporary folder inside home cannot tell the two apart.
            std::fs::remove_dir_all(root).unwrap();
            return;
        }
        let new = format!("haarness-tilde-{}", uuid::Uuid::now_v7());
        for (command, expected) in [
            (format!("touch ~/{new}"), glob(&home)),
            (format!("echo x >> ~/{new}"), glob(&home)),
            (format!("cat ~/{new}/$name"), glob(&home.join(&new))),
            ("cd ~".to_owned(), glob(&home)),
        ] {
            assert_eq!(
                analyze(&command, &workspace).external_dirs,
                vec![expected],
                "{command}"
            );
        }
        // Quoted, the shell keeps `~` as a name in the project.
        for command in [format!("touch '~/{new}'"), format!("touch \"~/{new}\"")] {
            assert!(
                analyze(&command, &workspace).external_dirs.is_empty(),
                "{command}"
            );
        }
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
