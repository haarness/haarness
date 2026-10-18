//! Shell-aware permission analysis for bash commands.
//! The command is parsed with tree-sitter-bash, every
//! `command` node yields an exact `bash:` operation plus a generalized
//! "always" prefix pattern, and path-taking commands / redirect targets that
//! escape the workspace raise `external_directory` operations.
//!
//! A command is also read the ways the shell runs it: without its leading
//! assignments, quotes, escapes or program folder (`FOO=1 "/bin/rm" x` is
//! `rm x`), through the wrappers that run the rest of their words (`sudo`,
//! `env`, `timeout`, `xargs`, `find -exec` …), and inside the text a shell
//! runs again (`bash -c`, `eval`, `env -S`, a here-document or here-string
//! fed to a shell or `source`). Each reading adds a pattern of its own (see
//! [`BashAnalysis::derived`]) and has its paths checked like a command
//! written on its own.

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
    /// The patterns read from inside a written command rather than written
    /// as one: another spelling of it (`git status` for `FOO=1 "git"
    /// status`), a command it runs (`rm x` for `sudo rm x`), or a command in
    /// text a shell runs again (`rm x` for `bash -c 'rm x'`). They are in
    /// [`Self::patterns`] too; their "always" patterns are not in
    /// [`Self::always`].
    pub derived: Vec<Derived>,
}

/// One pattern read from inside a written command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub pattern: String,
    /// The written command it was read from, as [`BashAnalysis::patterns`]
    /// holds it.
    pub written: String,
    /// Its generalized "don't ask again" pattern.
    pub always: String,
}

/// Commands whose non-flag arguments are filesystem paths worth resolving.
const PATH_COMMANDS: &[&str] = &[
    "cd", "rm", "cp", "mv", "mkdir", "touch", "chmod", "chown", "cat",
];

/// How many strings deep commands are read: `bash -c "eval 'sh -c …'"`.
const MAX_NESTING: u8 = 4;
/// Text a shell runs again that is longer than this is not read.
const MAX_NESTED_BYTES: usize = 256 * 1024;

/// Shells whose `-c` runs the word after their options, and which read a
/// script from standard input without one.
const SHELLS: &[&str] = &[
    "bash", "sh", "zsh", "dash", "ksh", "mksh", "ash", "fish", "yash", "posh",
];

/// Paths that name the reading command's own standard input.
const STDIN_PATHS: &[&str] = &["/dev/stdin", "/dev/fd/0", "/proc/self/fd/0"];

/// Analyze `command` as it would run from `workspace`. Parse failures fall
/// back to treating the whole command as one opaque pattern (never silently
/// allowing).
#[must_use]
pub fn analyze(command: &str, workspace: &Path) -> BashAnalysis {
    let mut reader = Reader {
        workspace,
        analysis: BashAnalysis::default(),
        written: Vec::new(),
        source: String::new(),
    };
    if !reader.read(command, 0) {
        return fallback(command);
    }
    let Reader {
        mut analysis,
        written,
        ..
    } = reader;
    if analysis.patterns.is_empty() && analysis.external_dirs.is_empty() {
        return fallback(command);
    }
    // A command written on its own is judged as written, whatever else it
    // is also read from.
    analysis
        .derived
        .retain(|derived| !written.contains(&derived.pattern));
    analysis
}

struct Reader<'w> {
    workspace: &'w Path,
    analysis: BashAnalysis,
    /// The patterns written as commands of their own.
    written: Vec<String>,
    /// The written command being read.
    source: String,
}

/// One word of a command: its program or an argument.
struct Word<'t> {
    node: tree_sitter::Node<'t>,
    /// As written.
    raw: String,
    /// As the program receives it, quotes and escapes removed, with
    /// expansions and substitutions as written.
    value: String,
    /// Whether `value` is all literal text.
    literal: bool,
}

/// Node kinds that are a command's words.
const WORD_KINDS: &[&str] = &[
    "command_name",
    "word",
    "number",
    "string",
    "raw_string",
    "ansi_c_string",
    "translated_string",
    "concatenation",
    "simple_expansion",
    "expansion",
    "command_substitution",
    "process_substitution",
    "arithmetic_expansion",
];

impl Reader<'_> {
    /// Read one shell program: the command as written (`depth` 0) or text
    /// inside it that a shell runs again. False when it does not parse.
    fn read(&mut self, command: &str, depth: u8) -> bool {
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_bash::LANGUAGE;
        if parser.set_language(&language.into()).is_err() {
            return false;
        }
        let Some(tree) = parser.parse(command, None) else {
            return false;
        };
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
                Some(parent) if parent.kind() == "redirected_statement" => {
                    node_text(parent, command)
                }
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
            if depth == 0 {
                self.source.clone_from(&command_text);
            }
            let words = words(node, command);
            self.check_paths(&words, command);
            if let Some(first) = tokens.first() {
                if first != "cd" {
                    let prefix = arity::prefix(&tokens).join(" ");
                    let always = format!("{prefix} *");
                    if depth == 0 {
                        push_unique(&mut self.analysis.patterns, command_text.clone());
                        push_unique(&mut self.analysis.always, always);
                        push_unique(&mut self.written, command_text);
                    } else {
                        self.derive(command_text, always);
                    }
                }
            }
            let assigned = (0..node.named_child_count())
                .filter_map(|index| node.named_child(index))
                .any(|child| child.kind() == "variable_assignment");
            self.spell(&words, assigned);
            self.unwrap(&words, Some(node), command, depth);
        }
        for redirect in redirects {
            let Some(dest) = redirect.child_by_field_name("destination") else {
                continue;
            };
            let (value, complete) = path_literal(dest, command);
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
            if let Some(dir) =
                external_dir(self.workspace, &node_text(dest, command), &value, complete)
            {
                push_unique(&mut self.analysis.external_dirs, glob(&dir));
            }
        }
        true
    }

    /// A pattern read from inside the written command being read.
    fn derive(&mut self, pattern: String, always: String) {
        if pattern.trim().is_empty() || pattern == self.source {
            return;
        }
        push_unique(&mut self.analysis.patterns, pattern.clone());
        let written = &self.source;
        if !self
            .analysis
            .derived
            .iter()
            .any(|derived| derived.pattern == pattern && derived.written == *written)
        {
            self.analysis.derived.push(Derived {
                pattern,
                written: written.clone(),
                always,
            });
        }
    }

    /// The other spellings of a command's words: as the program receives
    /// them, when that differs from how they are written or `changed` says
    /// the words are not the whole command (an assignment before them, a
    /// wrapper), and with the program's folder left out.
    fn spell(&mut self, words: &[Word<'_>], changed: bool) {
        let Some(program) = words.first() else {
            return;
        };
        if program.literal && program.value == "cd" {
            return;
        }
        let mut values: Vec<String> = words.iter().map(|word| word.value.clone()).collect();
        let spelled = values.join(" ");
        if changed
            || spelled
                != words
                    .iter()
                    .map(|word| word.raw.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
        {
            let always = format!("{} *", arity::prefix(&values).join(" "));
            self.derive(spelled, always);
        }
        if program.literal && program.value.contains(['/', '\\']) {
            let name = program
                .value
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or_default()
                .to_owned();
            if !name.is_empty() {
                values[0] = name;
                let always = format!("{} *", arity::prefix(&values).join(" "));
                self.derive(values.join(" "), always);
            }
        }
    }

    /// A command that a wrapper runs, read like one written on its own.
    fn invocation(
        &mut self,
        words: &[Word<'_>],
        stdin: Option<tree_sitter::Node<'_>>,
        source: &str,
        depth: u8,
    ) {
        if words.is_empty() {
            return;
        }
        self.check_paths(words, source);
        self.spell(words, true);
        self.unwrap(words, stdin, source, depth);
    }

    /// Text a shell runs again, read as a command of its own.
    fn nested(&mut self, text: &str, depth: u8) {
        if depth + 1 > MAX_NESTING || text.len() > MAX_NESTED_BYTES || text.trim().is_empty() {
            return;
        }
        self.read(text, depth + 1);
    }

    /// Read what a command runs: the rest of a wrapper's words, or the text
    /// a shell, `eval`, `env -S` or `source` runs again. `stdin` is the
    /// written command whose standard input the program reads.
    fn unwrap(
        &mut self,
        words: &[Word<'_>],
        stdin: Option<tree_sitter::Node<'_>>,
        source: &str,
        depth: u8,
    ) {
        let Some((program, arguments)) = words.split_first() else {
            return;
        };
        if !program.literal {
            return;
        }
        let input = || stdin.and_then(|node| stdin_script(node, source));
        let name = program_name(&program.value);
        match name.as_str() {
            "eval" => self.nested(&joined(arguments), depth),
            "source" | "." => {
                let text = arguments.first().and_then(|first| {
                    if first.node.kind() == "process_substitution" {
                        substituted(first.node, source)
                    } else if first.literal && STDIN_PATHS.contains(&first.value.as_str()) {
                        input()
                    } else {
                        None
                    }
                });
                if let Some(text) = text {
                    self.nested(&text, depth);
                }
            }
            "env" => {
                let options = Options::read(
                    arguments,
                    "uCPSa",
                    "",
                    &["unset", "chdir", "split-string", "argv0"],
                );
                let rest = after_assignments(options.rest);
                if let Some(split) = options.value("S").or_else(|| options.value("split-string")) {
                    // `env -S` splits its text into words, options and
                    // assignments included, and goes on with the words
                    // after it.
                    let rest: Vec<&str> =
                        options.rest.iter().map(|word| word.raw.as_str()).collect();
                    self.nested(&format!("env {split} {}", rest.join(" ")), depth);
                } else {
                    self.invocation(rest, stdin, source, depth);
                }
            }
            "sudo" | "doas" => {
                let options = Options::read(
                    arguments,
                    if name == "sudo" { "ugpCDrtTU" } else { "uC" },
                    "",
                    &[
                        "user",
                        "group",
                        "host",
                        "prompt",
                        "close-from",
                        "chdir",
                        "role",
                        "type",
                        "command-timeout",
                        "other-user",
                    ],
                );
                // `sudo -e` edits files, `-l` and `-V` list.
                if name == "sudo" && options.flags.contains(['e', 'l', 'V']) {
                    return;
                }
                let rest = after_assignments(options.rest);
                if rest.is_empty() {
                    // A shell that reads its commands from standard input.
                    if options.flags.contains(['s', 'i'])
                        && let Some(text) = input()
                    {
                        self.nested(&text, depth);
                    }
                    return;
                }
                self.invocation(rest, stdin, source, depth);
            }
            "su" => {
                if let Some(text) = su_command(arguments) {
                    self.nested(&text, depth);
                }
            }
            "command" => {
                let options = Options::read(arguments, "", "", &[]);
                // `command -v` and `-V` only say what a name is.
                if !options.flags.contains(['v', 'V']) {
                    self.invocation(options.rest, stdin, source, depth);
                }
            }
            "builtin" | "nohup" | "setsid" | "busybox" | "pkexec" => {
                let options = Options::read(arguments, "", "", &["user"]);
                self.invocation(options.rest, stdin, source, depth);
            }
            "exec" => {
                let options = Options::read(arguments, "a", "", &[]);
                self.invocation(options.rest, stdin, source, depth);
            }
            "nice" => {
                let options = Options::read(arguments, "n", "", &["adjustment"]);
                self.invocation(options.rest, stdin, source, depth);
            }
            "timeout" | "gtimeout" => {
                let options = Options::read(arguments, "sk", "", &["signal", "kill-after"]);
                // The first word after the options is the duration.
                if let Some((_, rest)) = options.rest.split_first() {
                    self.invocation(rest, stdin, source, depth);
                }
            }
            "time" => {
                let options = Options::read(arguments, "of", "", &["output", "format"]);
                self.invocation(options.rest, stdin, source, depth);
            }
            "stdbuf" => {
                let options = Options::read(arguments, "ioe", "", &["input", "output", "error"]);
                self.invocation(options.rest, stdin, source, depth);
            }
            "caffeinate" => {
                let options = Options::read(arguments, "tw", "", &[]);
                self.invocation(options.rest, stdin, source, depth);
            }
            "xargs" => {
                let options = Options::read(
                    arguments,
                    "InPLsdEaJRS",
                    "ile",
                    &[
                        "max-args",
                        "max-procs",
                        "max-lines",
                        "max-chars",
                        "delimiter",
                        "eof",
                        "arg-file",
                        "process-slot-var",
                    ],
                );
                // The command reads no input of the written command's.
                self.invocation(options.rest, None, source, depth);
            }
            "find" => {
                let mut index = 0;
                while index < arguments.len() {
                    let starts = matches!(
                        arguments[index].value.as_str(),
                        "-exec" | "-execdir" | "-ok" | "-okdir"
                    );
                    index += 1;
                    if !starts {
                        continue;
                    }
                    let start = index;
                    while index < arguments.len()
                        && !(arguments[index].value == ";"
                            || (arguments[index].value == "+"
                                && index > start
                                && arguments[index - 1].value == "{}"))
                    {
                        index += 1;
                    }
                    self.invocation(&arguments[start..index], None, source, depth);
                }
            }
            name if SHELLS.contains(&name) => {
                if let Some(text) = shell_script(arguments, input, source) {
                    self.nested(&text, depth);
                }
            }
            _ => {}
        }
    }

    /// The outside-the-project check for one command's words: the paths
    /// that the commands in [`PATH_COMMANDS`] take, by the program's name
    /// wherever it is installed.
    fn check_paths(&mut self, words: &[Word<'_>], source: &str) {
        let Some((program, arguments)) = words.split_first() else {
            return;
        };
        let (name, complete) = path_literal(program.node, source);
        let name = program_name(&name);
        if !complete || !PATH_COMMANDS.contains(&name.as_str()) {
            return;
        }
        for word in arguments {
            let (argument, complete) = path_literal(word.node, source);
            if argument.starts_with('-') || (name == "chmod" && argument.starts_with('+')) {
                continue;
            }
            if let Some(dir) = external_dir(self.workspace, &word.raw, &argument, complete) {
                push_unique(&mut self.analysis.external_dirs, glob(&dir));
            }
        }
    }
}

/// A command's words, its assignments and redirects left out.
fn words<'t>(node: tree_sitter::Node<'t>, source: &str) -> Vec<Word<'t>> {
    (0..node.named_child_count())
        .filter_map(|index| node.named_child(index))
        .filter(|child| WORD_KINDS.contains(&child.kind()))
        .map(|child| {
            let (value, literal) = unquote(child, source);
            Word {
                node: child,
                raw: node_text(child, source),
                value,
                literal,
            }
        })
        .collect()
}

/// Words as `eval` joins them before it runs them.
fn joined(words: &[Word<'_>]) -> String {
    words
        .iter()
        .map(|word| word.value.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The words after any `NAME=value` assignments (`env A=1 cmd`, `sudo A=1 cmd`).
fn after_assignments<'s, 't>(words: &'s [Word<'t>]) -> &'s [Word<'t>] {
    let count = words
        .iter()
        .take_while(|word| {
            word.literal
                && word.value.split_once('=').is_some_and(|(name, _)| {
                    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
        })
        .count();
    &words[count..]
}

/// A program's name as wrappers and shells are recognized: its last path
/// segment in lower case, without `.exe`.
fn program_name(value: &str) -> String {
    let name = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match name.strip_suffix(".exe") {
        Some(stem) => stem.to_owned(),
        None => name,
    }
}

/// A wrapper's own options, read the way getopt reads them.
struct Options<'s, 't> {
    /// The words after them: the command the wrapper runs.
    rest: &'s [Word<'t>],
    /// Short options given without a value.
    flags: String,
    /// Options given with a value, by their short letter or long name.
    values: Vec<(String, String)>,
}

impl<'s, 't> Options<'s, 't> {
    /// `valued` short options take a value (the rest of their cluster or the
    /// next word), `optional` ones only the rest of their cluster, and
    /// `long` ones the next word unless written `--name=value`.
    fn read(words: &'s [Word<'t>], valued: &str, optional: &str, long: &[&str]) -> Self {
        let mut options = Self {
            rest: words,
            flags: String::new(),
            values: Vec::new(),
        };
        let mut index = 0;
        while let Some(word) = words.get(index) {
            if !word.literal {
                break;
            }
            let value = word.value.as_str();
            if value == "--" {
                index += 1;
                break;
            }
            if let Some(name) = value.strip_prefix("--") {
                index += 1;
                match name.split_once('=') {
                    Some((name, given)) => options.values.push((name.into(), given.into())),
                    None if long.contains(&name) => {
                        let given = words.get(index).map(|word| word.value.clone());
                        options
                            .values
                            .push((name.into(), given.unwrap_or_default()));
                        index += 1;
                    }
                    None => {}
                }
                continue;
            }
            let Some(cluster) = value
                .strip_prefix('-')
                .filter(|cluster| !cluster.is_empty())
            else {
                break;
            };
            index += 1;
            for (at, letter) in cluster.char_indices() {
                let attached = &cluster[at + letter.len_utf8()..];
                if valued.contains(letter) {
                    let given = if attached.is_empty() {
                        index += 1;
                        words.get(index - 1).map(|word| word.value.clone())
                    } else {
                        Some(attached.to_owned())
                    };
                    options
                        .values
                        .push((letter.into(), given.unwrap_or_default()));
                    break;
                }
                if optional.contains(letter) {
                    options.values.push((letter.into(), attached.to_owned()));
                    break;
                }
                options.flags.push(letter);
            }
        }
        options.rest = &words[index.min(words.len())..];
        options
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(given, _)| given == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The text a shell runs: the word after its options under `-c`, or what
/// it reads from standard input (`input`) or a process substitution when it
/// is given no script file.
fn shell_script(
    arguments: &[Word<'_>],
    input: impl FnOnce() -> Option<String>,
    source: &str,
) -> Option<String> {
    let mut index = 0;
    let mut command = false;
    let mut from_stdin = false;
    while let Some(word) = arguments.get(index) {
        if !word.literal {
            break;
        }
        let value = word.value.as_str();
        if value == "--" || value == "-" {
            index += 1;
            break;
        }
        if let Some(name) = value.strip_prefix("--") {
            // fish spells `-c` as `--command`.
            if let Some(text) = name.strip_prefix("command=") {
                return Some(text.to_owned());
            }
            if name == "command" {
                return arguments.get(index + 1).map(|word| word.value.clone());
            }
            index += if matches!(name, "rcfile" | "init-file") {
                2
            } else {
                1
            };
            continue;
        }
        let Some(flags) = value
            .strip_prefix(['-', '+'])
            .filter(|flags| !flags.is_empty())
        else {
            break;
        };
        // A cluster such as `-lc`, `-xc` or `-ec` runs the next word as
        // `-c` does.
        if value.starts_with('-') {
            command |= flags.contains('c');
            from_stdin |= flags.contains('s');
        }
        // `-o name` and `-O name` take the next word.
        index += if flags.contains(['o', 'O']) { 2 } else { 1 };
    }
    let operand = arguments.get(index);
    if command {
        return operand.map(|word| word.value.clone());
    }
    match operand {
        None => input(),
        Some(_) if from_stdin => input(),
        Some(word) if word.node.kind() == "process_substitution" => substituted(word.node, source),
        Some(word) if word.literal && STDIN_PATHS.contains(&word.value.as_str()) => input(),
        // A script file, which is not read.
        Some(_) => None,
    }
}

/// The text `su -c` (or `--command`) runs, wherever the option stands.
fn su_command(arguments: &[Word<'_>]) -> Option<String> {
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        let value = word.value.as_str();
        if let Some(text) = value.strip_prefix("--command=") {
            return Some(text.to_owned());
        }
        if value == "--command"
            || (value.starts_with('-') && !value.starts_with("--") && value.ends_with('c'))
        {
            return words.next().map(|word| word.value.clone());
        }
    }
    None
}

/// The script a command reads on standard input, when the command line
/// spells it out: a here-string or here-document of its own, or what an
/// `echo`, `printf` or `cat` of one writes into a pipe to it.
fn stdin_script(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    if let Some(text) = children(node)
        .into_iter()
        .find(|child| child.kind() == "herestring_redirect")
        .and_then(|redirect| here_string(redirect, source))
    {
        return Some(text);
    }
    let parent = node.parent()?;
    match parent.kind() {
        "redirected_statement" => {
            children(parent)
                .into_iter()
                .find_map(|child| match child.kind() {
                    "heredoc_redirect" => heredoc_body(child, source),
                    "herestring_redirect" => here_string(child, source),
                    _ => None,
                })
        }
        "pipeline" => {
            if let Some(previous) = node.prev_named_sibling() {
                return output(previous, source);
            }
            // `cat <<EOF | sh`: the pipe is part of the here-document's
            // redirect, and `cat` writes the document into it.
            let redirect = parent
                .parent()
                .filter(|redirect| redirect.kind() == "heredoc_redirect")?;
            let writer = redirect.parent()?.child_by_field_name("body")?;
            let words = words(writer, source);
            let (program, arguments) = words.split_first()?;
            (program.literal && program_name(&program.value) == "cat" && arguments.is_empty())
                .then(|| heredoc_body(redirect, source))
                .flatten()
        }
        _ => None,
    }
}

fn children(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    (0..node.named_child_count())
        .filter_map(|index| node.named_child(index))
        .collect()
}

fn here_string(redirect: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let value = (0..redirect.named_child_count())
        .filter_map(|index| redirect.named_child(index))
        .find(|child| WORD_KINDS.contains(&child.kind()))?;
    Some(unquote(value, source).0)
}

fn heredoc_body(redirect: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    (0..redirect.named_child_count())
        .filter_map(|index| redirect.named_child(index))
        .find(|child| child.kind() == "heredoc_body")
        .map(|body| node_text(body, source))
}

/// What a process substitution writes, when its words say so.
fn substituted(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    (node.named_child_count() == 1)
        .then(|| node.named_child(0))
        .flatten()
        .and_then(|statement| output(statement, source))
}

/// What a command writes, when its words say so: `echo` and `printf` their
/// arguments, `cat` its here-document or here-string.
fn output(statement: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let command = match statement.kind() {
        "command" => statement,
        "redirected_statement" => statement
            .child_by_field_name("body")
            .filter(|body| body.kind() == "command")?,
        _ => return None,
    };
    let words = words(command, source);
    let (program, arguments) = words.split_first()?;
    if !program.literal {
        return None;
    }
    match program_name(&program.value).as_str() {
        "echo" => {
            let count = arguments
                .iter()
                .take_while(|word| {
                    word.value.len() > 1
                        && word.value.starts_with('-')
                        && word.value[1..]
                            .chars()
                            .all(|c| matches!(c, 'n' | 'e' | 'E'))
                })
                .count();
            Some(joined(&arguments[count..]))
        }
        "printf" => Some(joined(arguments).replace("\\n", "\n")),
        "cat" if arguments.is_empty() => stdin_script(command, source),
        _ => None,
    }
}

/// A word as the program receives it: quotes and escapes removed, with
/// expansions and substitutions kept as written (and the word marked not
/// literal).
fn unquote(node: tree_sitter::Node<'_>, source: &str) -> (String, bool) {
    match node.kind() {
        "raw_string" | "word" | "number" | "string_content" => literal(node, source),
        "ansi_c_string" => {
            let text = node_text(node, source);
            let inner = text
                .strip_prefix("$'")
                .and_then(|rest| rest.strip_suffix('\''))
                .unwrap_or_default();
            (ansi_c(inner), true)
        }
        "command_name" | "concatenation" | "string" | "translated_string" => {
            let mut value = String::new();
            let mut whole = true;
            for index in 0..node.named_child_count() {
                let Some(child) = node.named_child(index) else {
                    continue;
                };
                let (part, complete) = unquote(child, source);
                value.push_str(&part);
                whole &= complete;
            }
            (value, whole)
        }
        _ => (node_text(node, source), false),
    }
}

/// The text of `$'…'` as the shell reads it, its C escapes decoded.
fn ansi_c(text: &str) -> String {
    let mut value = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    // Up to `limit` digits of `radix` after an escape letter, as one character.
    let number = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
                  radix: u32,
                  limit: usize,
                  first: Option<u32>| {
        let mut code = first.unwrap_or(0);
        let mut count = usize::from(first.is_some());
        while count < limit {
            let Some(digit) = chars.peek().and_then(|c| c.to_digit(radix)) else {
                break;
            };
            code = code * radix + digit;
            chars.next();
            count += 1;
        }
        (count > 0).then(|| char::from_u32(code)).flatten()
    };
    while let Some(c) = chars.next() {
        if c != '\\' {
            value.push(c);
            continue;
        }
        let Some(next) = chars.next() else {
            value.push('\\');
            break;
        };
        let decoded = match next {
            'a' => Some('\x07'),
            'b' => Some('\x08'),
            'e' | 'E' => Some('\x1b'),
            'f' => Some('\x0c'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'v' => Some('\x0b'),
            '\\' | '\'' | '"' | '?' => Some(next),
            'x' => number(&mut chars, 16, 2, None),
            'u' => number(&mut chars, 16, 4, None),
            'U' => number(&mut chars, 16, 8, None),
            '0'..='7' => number(&mut chars, 8, 3, next.to_digit(8)),
            'c' => chars
                .next()
                .and_then(|control| u8::try_from(control).ok())
                .map(|byte| char::from(byte & 0x1f)),
            _ => None,
        };
        match decoded {
            Some(decoded) => value.push(decoded),
            None => {
                value.push('\\');
                value.push(next);
            }
        }
    }
    value
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
        ..BashAnalysis::default()
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
    let home = haarness_protocol::home_dir()
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

/// Variables that name a folder outside the project, which the command's
/// shell inherits from the server.
const FOLDER_VARIABLES: [&str; 6] = [
    "HOME",
    "TMPDIR",
    "HAARNESS_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
];

/// The folder `$NAME` or `${NAME}` stands for, when it is one of
/// [`FOLDER_VARIABLES`] and set to an absolute path.
fn folder_variable(text: &str) -> Option<String> {
    let name = text
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .or_else(|| text.strip_prefix('$'))?;
    if !FOLDER_VARIABLES.contains(&name) {
        return None;
    }
    std::env::var(name)
        .ok()
        .filter(|value| Path::new(value).is_absolute())
}

/// [`literal`] for a path argument or redirect target: a leading `$HOME`,
/// `${HOME}` or other [`FOLDER_VARIABLES`] entry, bare or in double quotes,
/// reads as the folder the shell puts there, so `> $HOME/.profile` is judged
/// as the file it writes, like `~/.profile`.
fn path_literal(node: tree_sitter::Node<'_>, source: &str) -> (String, bool) {
    match node.kind() {
        "simple_expansion" | "expansion" => folder_variable(&node_text(node, source))
            .map_or_else(|| (String::new(), false), |folder| (folder, true)),
        "concatenation" | "string" => {
            let mut value = String::new();
            for index in 0..node.named_child_count() {
                let Some(child) = node.named_child(index) else {
                    continue;
                };
                let (part, complete) = if index == 0 {
                    path_literal(child, source)
                } else {
                    literal(child, source)
                };
                value.push_str(&part);
                if !complete {
                    return (value, false);
                }
            }
            (value, true)
        }
        _ => literal(node, source),
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
        // The quoted message is also read as git receives it.
        assert_eq!(
            analysis.patterns,
            vec![
                "git add -A",
                "git commit -m 'x'",
                "git commit -m x",
                "echo failed"
            ]
        );
        assert_eq!(analysis.always, vec!["git add *", "git commit *", "echo *"]);
        assert_eq!(
            analysis.derived,
            vec![Derived {
                pattern: "git commit -m x".into(),
                written: "git commit -m 'x'".into(),
                always: "git commit *".into(),
            }]
        );
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
    fn a_leading_home_variable_is_the_home_folder() {
        let root = std::env::temp_dir().join(format!("haarness-home-var-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let Some(home) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
        else {
            std::fs::remove_dir_all(root).unwrap();
            return;
        };
        let home = resolve(&home, "");
        if workspace.canonicalize().unwrap().starts_with(&home) {
            std::fs::remove_dir_all(root).unwrap();
            return;
        }
        let new = format!("haarness-home-var-{}", uuid::Uuid::now_v7());
        for (command, expected) in [
            (format!("touch $HOME/{new}"), glob(&home)),
            (format!("echo x > \"${{HOME}}/{new}\""), glob(&home)),
            (
                format!("cp a.json $HOME/{new}/b.json"),
                glob(&home.join(&new)),
            ),
            ("cd $HOME".to_owned(), glob(&home)),
            ("rm -rf \"$HOME\"".to_owned(), glob(&home)),
            (format!("cat $HOME/{new}/$name"), glob(&home.join(&new))),
        ] {
            assert_eq!(
                analyze(&command, &workspace).external_dirs,
                vec![expected],
                "{command}"
            );
        }
        // Single quotes keep the text, and an expansion with an operator or
        // an unknown variable stays unjudged, as before.
        for command in [
            format!("touch '$HOME/{new}'"),
            format!("touch ${{HOME:-x}}/{new}"),
            format!("touch $PROJECT_OUT/{new}"),
            format!("touch sub$HOME/{new}"),
        ] {
            assert!(
                analyze(&command, &workspace).external_dirs.is_empty(),
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

#[cfg(test)]
mod readings {
    use super::*;
    use crate::permissions::wildcard_match;

    fn workspace() -> PathBuf {
        std::env::current_dir().expect("cwd")
    }

    /// The patterns read from inside `command`.
    fn derived(command: &str) -> Vec<String> {
        let analysis = analyze(command, &workspace());
        for derived in &analysis.derived {
            assert!(
                analysis.patterns.contains(&derived.pattern)
                    && analysis.patterns.contains(&derived.written),
                "{command}: {analysis:?}"
            );
        }
        analysis
            .derived
            .into_iter()
            .map(|derived| derived.pattern)
            .collect()
    }

    fn quoted(text: &str) -> String {
        format!("'{}'", text.replace('\'', "'\\''"))
    }

    #[test]
    fn a_rule_for_rm_reaches_every_command_that_runs_rm() {
        for command in [
            "bash -c \"rm -rf x\"",
            "sh -xc 'rm x'",
            "bash -lc 'rm x'",
            "bash -ec 'rm x'",
            "bash -cx 'rm x'",
            "zsh -c 'rm x'",
            "fish --command='rm x'",
            "bash -o pipefail -c 'rm x'",
            "bash --norc -c 'rm x'",
            "/bin/bash -c 'rm x'",
            "eval \"rm x\"",
            "eval rm x",
            "eval 'rm' \"x\"",
            "eval $'rm x'",
            "eval $'\\x72m x'",
            "env -S 'rm x'",
            "env -iS 'rm x'",
            "env --split-string='rm x'",
            "env -S'rm x'",
            "env -i A=1 rm x",
            "env -u HOME -- rm x",
            "xargs rm",
            "xargs -0 -I{} rm {}",
            "xargs -n 1 -P 4 rm",
            "find . -exec rm {} +",
            "find . -name '*.tmp' -execdir rm {} \\;",
            "xargs sh -c 'rm \"$@\"' _",
            "find . -exec sh -c 'rm x' \\;",
            "sudo rm x",
            "sudo -u root -E rm x",
            "sudo FOO=1 rm x",
            "doas rm x",
            "nohup rm x",
            "timeout 5 rm x",
            "timeout -s KILL -k 1 5 rm x",
            "nice -n 5 rm x",
            "nice -10 rm x",
            "time rm x",
            "time -p rm x",
            "command rm x",
            "command -p rm x",
            "exec rm x",
            "exec -a name rm x",
            "stdbuf -oL rm x",
            "setsid rm x",
            "caffeinate -i rm x",
            "busybox rm x",
            "/bin/rm x",
            "\\rm x",
            "\"rm\" x",
            "'rm' x",
            "r\"m\" x",
            "FOO=1 rm x",
            "sudo FOO=1 timeout 5 nice -n 2 rm x",
            "su -c 'rm x'",
            "su root -c 'rm x'",
            "su - root --command='rm x'",
            "bash <<< 'rm x'",
            "bash <<'EOF'\nrm x\nEOF",
            "sudo bash <<EOF\nrm x\nEOF",
            "sh -s <<EOF\nrm x\nEOF",
            "source /dev/stdin <<< 'rm x'",
            ". /dev/stdin <<EOF\nrm x\nEOF",
            "echo 'rm x' | sh",
            "printf 'rm x\\n' | bash",
            "cat <<EOF | sh\nrm x\nEOF",
            "source <(echo 'rm x')",
            ". <(printf 'rm x')",
            "bash <(echo rm x)",
            "bash -c \"eval 'sh -c \\\"rm x\\\"'\"",
            "busybox sh -c 'rm x'",
            "git status && sudo rm x",
            "for f in a; do timeout 1 rm \"$f\"; done",
        ] {
            let readings = derived(command);
            assert!(
                readings
                    .iter()
                    .any(|pattern| wildcard_match("rm *", pattern)),
                "{command}: {readings:?}"
            );
        }
    }

    #[test]
    fn words_that_only_name_rm_run_nothing() {
        for command in [
            "command -v rm",
            "echo rm x",
            "printf 'rm x'",
            "bash script.sh rm",
            "sudo -l rm",
            "git commit -m 'rm x'",
            "cat <<EOF\nrm x\nEOF",
            "grep -r 'eval rm' .",
            "echo 'rm x' > script.sh",
            "find . -name rm",
        ] {
            let readings = derived(command);
            assert!(
                !readings
                    .iter()
                    .any(|pattern| wildcard_match("rm *", pattern)),
                "{command}: {readings:?}"
            );
        }
    }

    #[test]
    fn plain_commands_read_no_more_than_they_say() {
        for command in [
            "git status --short",
            "cargo test -p haarness-core",
            "ls -la src",
            "npm run build",
            "git log --oneline -5 | head",
            "echo hi > out.txt",
            "cd src && make",
            "git commit -m x",
            "cat <<EOF > notes.txt\nhello\nEOF",
        ] {
            assert_eq!(derived(command), Vec::<String>::new(), "{command}");
        }
    }

    #[test]
    fn other_spellings_of_a_command_are_read_as_the_shell_runs_them() {
        for (command, reading, always) in [
            ("FOO=1 git status", "git status", "git status *"),
            ("\"git\" status", "git status", "git status *"),
            ("\\git status", "git status", "git status *"),
            ("/usr/bin/git status", "git status", "git status *"),
            ("git push \"--force\"", "git push --force", "git push *"),
        ] {
            let analysis = analyze(command, &workspace());
            assert_eq!(analysis.patterns[0], command);
            let derived = analysis
                .derived
                .iter()
                .find(|derived| derived.pattern == reading)
                .unwrap_or_else(|| panic!("{command}: {analysis:?}"));
            assert_eq!(derived.written, command);
            assert_eq!(derived.always, always);
        }
        // The command as written keeps its own "always" pattern only.
        assert_eq!(
            analyze("FOO=1 git status", &workspace()).always,
            vec!["git status *"]
        );
    }

    #[test]
    fn a_command_written_on_its_own_is_never_a_reading() {
        let analysis = analyze("rm x; sudo rm x", &workspace());
        assert_eq!(analysis.patterns, vec!["rm x", "sudo rm x"]);
        assert!(
            analysis
                .derived
                .iter()
                .all(|derived| derived.pattern != "rm x"),
            "{analysis:?}"
        );
    }

    #[test]
    fn nesting_and_size_are_bounded() {
        let mut command = "rm x".to_owned();
        for _ in 0..MAX_NESTING {
            command = format!("bash -c {}", quoted(&command));
        }
        assert!(derived(&command).iter().any(|pattern| pattern == "rm x"));
        let deeper = format!("bash -c {}", quoted(&command));
        assert!(!derived(&deeper).iter().any(|pattern| pattern == "rm x"));
        let long = format!(
            "bash -c '{}rm x'",
            "true; ".repeat(MAX_NESTED_BYTES / 6 + 1)
        );
        assert!(!derived(&long).iter().any(|pattern| pattern == "rm x"));
    }

    #[test]
    fn ansi_c_strings_decode_like_the_shell() {
        assert_eq!(
            ansi_c("\\x72m\\t\\101\\cA\\q\\u00e9"),
            "rm\tA\u{1}\\q\u{e9}"
        );
        assert_eq!(ansi_c("\\x"), "\\x");
        assert_eq!(ansi_c("it\\'s"), "it's");
    }

    #[test]
    fn paths_in_the_commands_a_command_runs_are_checked() {
        let root = std::env::temp_dir().join(format!("haarness-readings-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("project");
        let outside = root.join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let expected = vec![glob(&outside.canonicalize().unwrap())];
        let outside = outside.display();
        for command in [
            format!("sudo rm -rf {outside}/x"),
            format!("timeout 5 cat {outside}/f"),
            format!("/bin/rm {outside}/x"),
            format!("bash -c 'touch {outside}/new'"),
            format!("sh -c \"echo x > {outside}/new\""),
            format!("env -S 'touch {outside}/new'"),
            format!("eval touch {outside}/new"),
            format!("xargs rm {outside}/x"),
            format!("find . -exec cp {{}} {outside}/ \\;"),
            format!("bash <<EOF\ntouch {outside}/new\nEOF"),
            format!("echo 'mkdir {outside}/made' | sh"),
        ] {
            assert_eq!(
                analyze(&command, &workspace).external_dirs,
                expected,
                "{command}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
