//! Ordered, last-match-wins permission rules over `permission:pattern`
//! operation strings, with wildcard semantics and
//! independently-evaluated ceilings.
//!
//! Ordinary tool defaults: everything runs (`*` = allow),
//! with prompts for external paths and environment secrets. Haarness also
//! asks for loop protection, agent changes, and generated media. Tightening
//! is user configuration; "Allow always"
//! learns generalized command-prefix patterns (see [`bash`]/[`arity`]).

pub mod arity;
pub mod bash;
pub mod script;

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub pattern: String,
    pub decision: PermissionDecision,
}

/// The outcome of evaluating one operation, carrying the matched rule so
/// config denials can tell the model exactly which rule fired (and that
/// retrying is pointless).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub decision: PermissionDecision,
    pub matched: Option<String>,
    /// The ask is about one of Haarness's own configuration files (see
    /// [`is_config_path`]): only a person answers it, never Auto, a
    /// `--yolo` or scheduled run, or a hook, and it offers no "always".
    pub protected: bool,
    /// A rule someone configured decided (a person's, a project's or an
    /// agent's), not one of Haarness's defaults: Auto's reviewer leaves a
    /// configured ask about something narrower than a whole tool to a person.
    pub configured: bool,
}

/// The rule an edit of a configuration file is asked under, as
/// [`Evaluation::matched`] names it.
pub const CONFIG_EDIT_RULE: &str = "edit:<haarness configuration>";

/// Whether `path` (after `.` and `..`, at any depth) is a file Haarness, or a
/// tool it reads settings from, takes permissions, hooks, MCP servers,
/// extensions or providers from: `haarness.json{,c}`, `opencode.json{,c}`,
/// `.haarness/config.toml`, `.haarness/extensions.json` and the extension code
/// under `.haarness/extensions/`, `.claude/settings.json`,
/// `.claude/settings.local.json`, `.mcp.json`, a plugin's `hooks/hooks.json`
/// and `.claude-plugin/plugin.json`, the global configuration folder's
/// `config.toml`, `extensions.json`, `extensions/` and `prompts/`, and every
/// file of the system configuration folder (`HAARNESS_SYSTEM_CONFIG_DIR`). An
/// agent that changes one could widen its own permissions, or rewrite its own
/// instructions, at the next reload, so an edit asks a person unless a rule
/// naming the file allows it. Names compare without case and without the
/// trailing dots and spaces Windows drops.
#[must_use]
pub fn is_config_path(path: &Path) -> bool {
    let path = normalize_absolute(path);
    let names: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(file_name_key(&name.to_string_lossy())),
            _ => None,
        })
        .collect();
    let Some((name, folders)) = names.split_last() else {
        return false;
    };
    let parent = folders.last().map(String::as_str);
    if matches!(
        name.as_str(),
        "haarness.json" | "haarness.jsonc" | "opencode.json" | "opencode.jsonc" | ".mcp.json"
    ) {
        return true;
    }
    let pair = (parent.unwrap_or_default(), name.as_str());
    if matches!(
        pair,
        (".haarness", "config.toml" | "extensions.json")
            | (".claude", "settings.json" | "settings.local.json")
            | ("hooks", "hooks.json")
            | (".claude-plugin", "plugin.json")
    ) {
        return true;
    }
    // Extension code, which runs inside Haarness and can answer approvals.
    if folders
        .windows(2)
        .any(|pair| pair[0] == ".haarness" && pair[1] == "extensions")
    {
        return true;
    }
    global_config_dir().is_some_and(|global| is_global_config_path(&path, &global))
        || system_config_dir().is_some_and(|base| is_system_config_path(&path, &base))
}

/// Whether `path` is one of the global configuration folder's own files:
/// `config.toml`, `extensions.json`, `haarness.json{,c}`, `opencode.json{,c}`,
/// extension code under `extensions/` or a prompt file under `prompts/`. The
/// folder and each of those may be a link (dotfiles kept elsewhere and linked
/// in, as `stow` does), so the files they really lead to count too.
#[must_use]
pub fn is_global_config_path(path: &Path, global: &Path) -> bool {
    const FILES: [&str; 6] = [
        "config.toml",
        "extensions.json",
        "haarness.json",
        "haarness.jsonc",
        "opencode.json",
        "opencode.jsonc",
    ];
    let path = normalize_absolute(path);
    let real_path = path.canonicalize().ok();
    let names = |candidate: &Path| path == candidate || real_path.as_deref() == Some(candidate);
    let global = normalize_absolute(global);
    let roots = [Some(global.clone()), global.canonicalize().ok()];
    // A path inside the folder, as written or through the folder's link.
    let inside = roots.iter().flatten().any(|root| {
        [Some(&path), real_path.as_ref()]
            .into_iter()
            .flatten()
            .any(|path| {
                path.strip_prefix(root).is_ok_and(|rest| {
                    let rest: Vec<String> = rest
                        .components()
                        .map(|part| file_name_key(&part.as_os_str().to_string_lossy()))
                        .collect();
                    match rest.as_slice() {
                        [name] => FILES.contains(&name.as_str()),
                        [folder, _, ..] => folder == "extensions" || folder == "prompts",
                        [] => false,
                    }
                })
            })
    });
    // Or where one of the files, or the extensions or prompts folder, really is.
    inside
        || FILES
            .iter()
            .filter_map(|name| global.join(name).canonicalize().ok())
            .any(|real| names(&real))
        || ["extensions", "prompts"].iter().any(|folder| {
            global.join(folder).canonicalize().is_ok_and(|real| {
                [Some(&path), real_path.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|path| path.starts_with(&real) && path != &real)
            })
        })
}

/// Whether `path` is inside the system configuration folder, as written or
/// where the folder (or the file) really is: all of it is configuration that
/// a team or device management ships, and Haarness only reads it.
#[must_use]
pub fn is_system_config_path(path: &Path, base: &Path) -> bool {
    let path = normalize_absolute(path);
    let real_path = path.canonicalize().ok();
    let base = normalize_absolute(base);
    [Some(base.clone()), base.canonicalize().ok()]
        .iter()
        .flatten()
        .any(|root| {
            [Some(&path), real_path.as_ref()]
                .into_iter()
                .flatten()
                .any(|path| path.starts_with(root) && path != root)
        })
}

/// A file or folder name as Haarness compares it: lower case, without the
/// trailing dots and spaces Windows drops when it creates the file.
fn file_name_key(name: &str) -> String {
    name.trim_end_matches(['.', ' ']).to_ascii_lowercase()
}

static GLOBAL_CONFIG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Record the folder this process reads its global configuration from (the
/// CLI's resolved layout), so [`is_config_path`] protects exactly that one.
/// The first call wins.
pub fn set_global_config_dir(dir: PathBuf) {
    let _ = GLOBAL_CONFIG_DIR.set(dir);
}

static SYSTEM_CONFIG_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Record the system configuration folder this process reads, if any (the
/// CLI's resolved `HAARNESS_SYSTEM_CONFIG_DIR`), so [`is_config_path`] protects
/// all of it. The first call wins.
pub fn set_system_config_dir(dir: Option<PathBuf>) {
    let _ = SYSTEM_CONFIG_DIR.set(dir);
}

/// The system configuration folder: the one the CLI recorded, else the
/// process environment's; never the machine's own in this crate's tests.
fn system_config_dir() -> Option<PathBuf> {
    if let Some(dir) = SYSTEM_CONFIG_DIR.get() {
        return dir.clone();
    }
    if cfg!(test) {
        return None;
    }
    haarness_protocol::system_config_dir()
}

/// The global configuration folder: the one the CLI recorded, else
/// [`global_config_dir_from`] the process environment.
fn global_config_dir() -> Option<PathBuf> {
    if let Some(dir) = GLOBAL_CONFIG_DIR.get() {
        return Some(dir.clone());
    }
    global_config_dir_from(&|name| std::env::var_os(name))
}

/// The global configuration folder for an environment, resolved as the
/// CLI's `Paths` does: `$HAARNESS_DATA_DIR/config`, `$HAARNESS_HOME/config`,
/// `$XDG_CONFIG_HOME/haarness`, then `.config/haarness` in `$HOME` (or, on
/// Windows, `%USERPROFILE%`).
#[must_use]
pub fn global_config_dir_from(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let set = |name: &str| {
        env(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(root) = set("HAARNESS_DATA_DIR") {
        return Some(root.join("config"));
    }
    if let Some(root) = set("HAARNESS_HOME").filter(|path| path.is_absolute()) {
        return Some(root.join("config"));
    }
    set("XDG_CONFIG_HOME")
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env("HOME")
                .map(PathBuf::from)
                .or_else(|| set("USERPROFILE"))
                .map(|home| home.join(".config"))
        })
        .map(|base| base.join("haarness"))
}

/// Whether a rule's pattern covers every file rather than naming some: `*`,
/// `edit:*`, `edit:**` or `edit:**/*`. Such a rule (or an "always" answer,
/// which is `edit:*`) does not open a configuration file.
fn catch_all(rule: &PermissionRule) -> bool {
    rule.pattern == "*"
        || rule.pattern.split_once(':').is_some_and(|(_, target)| {
            target
                .chars()
                .all(|character| matches!(character, '*' | '/'))
        })
}

/// Whether an allow rule opens the protected file `name`: its last path
/// segment holds the name literally, as `haarness.json`, `**/haarness.json`
/// and `*.mcp.json` do, while `*.json`, `.claude/**` and `edit:*` do not.
fn names_file(rule: &PermissionRule, name: &str) -> bool {
    !catch_all(rule)
        && rule.pattern.split_once(':').is_some_and(|(_, target)| {
            let last = target.rsplit(['/', '\\']).next().unwrap_or(target);
            !name.is_empty() && file_name_key(last).contains(name)
        })
}

#[derive(Debug, Clone, Default)]
pub struct PermissionEngine {
    rules: Vec<PermissionRule>,
    /// How many of `rules`, from the start, are Haarness's defaults.
    builtin: usize,
    repository_grants: Vec<PermissionRule>,
    session_rules: Vec<PermissionRule>,
    /// Evaluated independently of the rules: a ceiling `deny` wins even when
    /// a later rule or session approval allows. The agent's own (the plan
    /// agent's write boundary).
    ceilings: Vec<PermissionRule>,
    /// The boundaries a child agent inherits from the chats above it, each
    /// one ruleset evaluated on its own, so one that ends with a broad allow
    /// never reopens another's deny. Unlike the agent's own ceiling, they
    /// differ between sessions of one agent, so they refuse calls but never
    /// take a tool out of the list the model sees.
    inherited: Vec<Vec<PermissionRule>>,
    /// Each configured `external_directory` rule whose folder is a link, in
    /// the form the asks use (the folder it leads to), from the
    /// process-wide [`configured_real_folder`]: never resolved on a check.
    real_folders: std::collections::BTreeMap<String, String>,
    /// The project folder, when known: `read`, `edit` and `list` rules then
    /// name a project file however the call spelled its path.
    workspace: Option<PathBuf>,
}

/// Permissions whose patterns are file paths. A rule for one of them written
/// relative to the project also names the file's absolute path, and an
/// absolute rule its project-relative one.
const PATH_PERMISSIONS: [&str; 3] = ["read", "edit", "list"];

/// The built-in `read` rules for environment secrets, in order (last match
/// wins). Packages ship `.env` files, often as test fixtures, so a file
/// inside a `node_modules` folder is read without asking; `*` crosses `/`,
/// so the two allows cover such a folder at any depth, in relative and
/// absolute paths alike.
const SECRET_READ_RULES: [(&str, PermissionDecision); 5] = [
    ("*.env", PermissionDecision::Ask),
    ("*.env.*", PermissionDecision::Ask),
    ("*.env.example", PermissionDecision::Allow),
    ("node_modules/*", PermissionDecision::Allow),
    ("*/node_modules/*", PermissionDecision::Allow),
];

/// Whether the built-in read rules ask before reading this path, after `.`
/// and `..` are resolved lexically. Search tools leave such a secret-named
/// file out and only say how many they left out; an explicit read of one
/// still asks. Configured rules play no part, so every search hides the same
/// files.
#[must_use]
pub fn is_secret_path(path: &Path) -> bool {
    let path = normalize_absolute(path);
    let path = path.to_string_lossy();
    SECRET_READ_RULES
        .iter()
        .rfind(|(pattern, _)| wildcard_match(pattern, &path))
        .is_some_and(|(_, decision)| *decision == PermissionDecision::Ask)
}

impl PermissionEngine {
    #[must_use]
    pub fn new(rules: Vec<PermissionRule>) -> Self {
        let mut engine = Self {
            rules: rules.into_iter().map(normalize_rule_aliases).collect(),
            builtin: 0,
            session_rules: Vec::new(),
            repository_grants: Vec::new(),
            ceilings: Vec::new(),
            inherited: Vec::new(),
            real_folders: std::collections::BTreeMap::new(),
            workspace: None,
        };
        let rules = engine.rules.clone();
        engine.learn(&rules);
        engine
    }

    /// Record where the configured `external_directory` rules among `rules`
    /// really lead (see `real_folders`), resolving each pattern once per
    /// process.
    fn learn(&mut self, rules: &[PermissionRule]) {
        self.learn_with(rules, configured_real_folder);
    }

    /// [`learn`](Self::learn) for rules that may mix configured rules with
    /// answers (a parent's boundary): only what the process already resolved
    /// for a configured rule is used, and nothing is followed now.
    fn learn_resolved(&mut self, rules: &[PermissionRule]) {
        self.learn_with(rules, resolved_real_folder);
    }

    fn learn_with(&mut self, rules: &[PermissionRule], real: fn(&str) -> Option<String>) {
        for rule in rules {
            if !self.real_folders.contains_key(&rule.pattern)
                && let Some(real) = real(&rule.pattern)
            {
                self.real_folders.insert(rule.pattern.clone(), real);
            }
        }
    }

    /// Let path rules name a file in `workspace` by either spelling: a model
    /// may read `/project/secrets/key` while the rule says `secrets/*`, or
    /// the other way round. Rules starting with `*` match as written.
    pub fn set_workspace(&mut self, workspace: &Path) {
        self.workspace = Some(workspace.to_path_buf());
    }

    /// Ordinary tools run by default. External paths and environment secrets
    /// require approval, alongside explicit agent changes and generated media.
    #[must_use]
    pub fn build_defaults() -> Self {
        let mut rules = vec![
            rule("*", PermissionDecision::Allow),
            // Switching the active primary agent changes the governing prompt
            // and permissions for future turns, so require an explicit user
            // approval even though ordinary tools auto-run.
            rule("agent_switch:*", PermissionDecision::Ask),
            // Generated media is an external, billable side effect and also
            // writes a workspace artifact. Keep it opt-in even under the
            // otherwise permissive default profile.
            rule("media_generate:*", PermissionDecision::Ask),
            rule("doom_loop:*", PermissionDecision::Ask),
            rule("external_directory:*", PermissionDecision::Ask),
            rule("browser_site:*", PermissionDecision::Ask),
        ];
        rules.extend(
            SECRET_READ_RULES
                .iter()
                .map(|(pattern, decision)| rule(&format!("read:{pattern}"), *decision)),
        );
        let mut engine = Self::new(rules);
        engine.builtin = engine.rules.len();
        engine
    }

    /// The plan agent gets the same base rules plus a write-denying ceiling
    /// (plan files excepted). As a ceiling it cannot be re-opened by user
    /// config or session approvals.
    #[must_use]
    pub fn plan_defaults() -> Self {
        let mut engine = Self::build_defaults();
        engine.ceilings = plan_ceiling();
        let ceilings = engine.ceilings.clone();
        engine.learn(&ceilings);
        engine
    }

    pub fn approve_always(&mut self, pattern: impl Into<String>) {
        // An answer names the folder an ask named, which is already where
        // the path really leads: it is never followed.
        self.session_rules
            .push(normalize_rule_aliases(PermissionRule {
                pattern: pattern.into(),
                decision: PermissionDecision::Allow,
            }));
    }
    pub fn add_rule(&mut self, rule: PermissionRule) {
        let rule = normalize_rule_aliases(rule);
        self.learn(std::slice::from_ref(&rule));
        self.rules.push(rule);
    }
    pub fn add_repository_grant(&mut self, rule: PermissionRule) {
        self.repository_grants.push(normalize_rule_aliases(rule));
    }
    /// An "always" answer (this session or this workspace). It settles what
    /// the configured rules leave as a question and never reopens their deny.
    /// Like a repository grant it names the folder its ask named, so it is
    /// never followed through a link.
    pub fn add_grant(&mut self, rule: PermissionRule) {
        self.session_rules.push(normalize_rule_aliases(rule));
    }
    pub fn add_ceiling(&mut self, rule: PermissionRule) {
        let rule = normalize_rule_aliases(rule);
        self.learn(std::slice::from_ref(&rule));
        self.ceilings.push(rule);
    }
    /// Bound this engine by a parent's [`boundary`](Self::boundary): each of
    /// its rulesets refuses what it denies, whatever the others allow.
    pub fn add_inherited_ceiling(
        &mut self,
        boundary: impl IntoIterator<Item = Vec<PermissionRule>>,
    ) {
        let boundary: Vec<Vec<PermissionRule>> = boundary
            .into_iter()
            .map(|rules| rules.into_iter().map(normalize_rule_aliases).collect())
            .collect();
        for rules in &boundary {
            self.learn_resolved(rules);
        }
        self.inherited.extend(boundary);
    }
    /// The effective ruleset (rules + session approvals), e.g. to snapshot a
    /// parent's boundary as a child agent's ceiling.
    #[must_use]
    pub fn flattened_rules(&self) -> Vec<PermissionRule> {
        // Configured rules last, so their deny stays final for a child too.
        self.repository_grants
            .iter()
            .chain(&self.session_rules)
            .chain(&self.rules)
            .cloned()
            .collect()
    }

    /// Everything that bounds this engine, as rulesets to evaluate one by
    /// one: its effective rules, its own ceiling and those it inherited. A
    /// child agent inherits this as its ceiling so it can never escalate
    /// past its parent.
    #[must_use]
    pub fn boundary(&self) -> Vec<Vec<PermissionRule>> {
        let mut boundary = vec![self.flattened_rules()];
        if !self.ceilings.is_empty() {
            boundary.push(self.ceilings.clone());
        }
        boundary.extend(self.inherited.iter().cloned());
        boundary
    }

    #[must_use]
    pub fn evaluate(&self, operation: &str) -> PermissionDecision {
        self.evaluate_detailed(operation).decision
    }

    #[must_use]
    pub fn evaluate_detailed(&self, operation: &str) -> Evaluation {
        self.evaluate_passing_over(operation, &|_| false, PermissionDecision::Ask)
    }

    /// The verdict on a pattern read from inside a written shell command
    /// (another spelling of it, or a command it runs). In every layer
    /// (ceilings, configured rules, remembered answers) the rules `covers`
    /// holds for are passed over: rules that also name the command as
    /// written, such as a blanket `*`, `"bash": "ask"` or an allow for the
    /// wrapper, judged that command already. What no other rule names is
    /// allowed. So a ceiling's blanket deny with an allow for `timeout *`
    /// leaves `cargo test` in `timeout 5 cargo test` alone, while a deny for
    /// `tool-x *` in any layer refuses `timeout 5 tool-x a`.
    #[must_use]
    pub fn evaluate_reading(&self, operation: &str, covers: &dyn Fn(&str) -> bool) -> Evaluation {
        self.evaluate_passing_over(operation, covers, PermissionDecision::Allow)
    }

    /// A bash rule that denies or asks without covering the command as
    /// written (`covers`), with its decision: a deny when there is one, in
    /// any layer, else an ask. A command whose readings a limit stopped is
    /// refused or asked about when one exists.
    #[must_use]
    pub fn narrower_bash_rule(
        &self,
        covers: &dyn Fn(&str) -> bool,
    ) -> Option<(String, PermissionDecision)> {
        let narrower = |decision: PermissionDecision| {
            std::iter::once(&self.ceilings)
                .chain(&self.inherited)
                .chain(std::iter::once(&self.rules))
                .flatten()
                .filter(|rule| rule.decision == decision)
                .filter(|rule| {
                    rule.pattern
                        .split_once(':')
                        .is_some_and(|(permission, _)| wildcard_match(permission, "bash"))
                })
                .find(|rule| !covers(&rule.pattern))
                .map(|rule| (rule.pattern.clone(), decision))
        };
        narrower(PermissionDecision::Deny).or_else(|| narrower(PermissionDecision::Ask))
    }

    fn evaluate_passing_over(
        &self,
        operation: &str,
        passed_over: &dyn Fn(&str) -> bool,
        unmatched: PermissionDecision,
    ) -> Evaluation {
        let operation = normalize_operation_alias(operation);
        let spellings = self.path_spellings(&operation);
        let names = |rule: &&PermissionRule| {
            rule_names(rule, &operation, &spellings, &self.real_folders)
                && !passed_over(&rule.pattern)
        };
        // Ceilings first and independently: their deny is final.
        for ceiling in std::iter::once(&self.ceilings).chain(&self.inherited) {
            if let Some(ceiling) = ceiling.iter().rfind(names)
                && ceiling.decision == PermissionDecision::Deny
            {
                return Evaluation {
                    decision: PermissionDecision::Deny,
                    matched: Some(ceiling.pattern.clone()),
                    protected: false,
                    configured: true,
                };
            }
        }
        let configured_at = self.rules.iter().rposition(|rule| names(&rule));
        let configured = configured_at.map(|index| &self.rules[index]);
        let grant = self.session_rules.iter().rfind(names);
        let repository = self.repository_grants.iter().rfind(names);
        // An "always" answer or sharing consent never reopens a configured
        // deny: `always` on `git push origin` must not allow a denied
        // `git push --force`.
        let matched = if configured.is_none_or(|rule| rule.decision != PermissionDecision::Deny) {
            repository.or(grant).or(configured)
        } else {
            configured
        };
        // An edit of a configuration file asks unless a deny refuses it or
        // an allow names it: a blanket `edit: allow`, an allow for a folder
        // or `*.json`, or an "always" answer to another edit never opens it.
        if let Some(target) = operation
            .split_once(':')
            .filter(|(permission, _)| *permission == "edit")
            .map(|(_, target)| target)
            // `~/.config/haarness/config.toml` is the file in the home folder.
            .filter(|target| is_config_path(Path::new(expand_home(target).as_ref())))
        {
            let name = Path::new(target)
                .file_name()
                .map(|name| file_name_key(&name.to_string_lossy()))
                .unwrap_or_default();
            // The last rule of each layer that counts here: an allow that
            // does not name the file is passed over.
            let counts = |rule: &&PermissionRule| {
                names(rule)
                    && (rule.decision != PermissionDecision::Allow || names_file(rule, &name))
            };
            let configured = self.rules.iter().rfind(counts);
            if let Some(rule) = configured.filter(|rule| rule.decision == PermissionDecision::Deny)
            {
                return Evaluation {
                    decision: PermissionDecision::Deny,
                    matched: Some(rule.pattern.clone()),
                    protected: false,
                    configured: true,
                };
            }
            let opener = [
                self.repository_grants.iter().rfind(counts),
                self.session_rules.iter().rfind(counts),
                configured,
            ]
            .into_iter()
            .flatten()
            .find(|rule| rule.decision == PermissionDecision::Allow);
            return Evaluation {
                decision: if opener.is_some() {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Ask
                },
                matched: Some(
                    opener.map_or_else(|| CONFIG_EDIT_RULE.into(), |rule| rule.pattern.clone()),
                ),
                protected: opener.is_none(),
                configured: opener.is_some(),
            };
        }
        // The rule that decided is a configured one when it is among the
        // rules after the defaults, rather than an answer someone gave.
        let from_rules = matched
            .zip(configured)
            .is_some_and(|(chosen, rule)| std::ptr::eq(chosen, rule));
        Evaluation {
            decision: matched.map_or(unmatched, |rule| rule.decision),
            matched: matched.map(|rule| rule.pattern.clone()),
            protected: false,
            configured: from_rules && configured_at.is_some_and(|index| index >= self.builtin),
        }
    }

    /// The other ways to write a path operation's file, each with whether it
    /// is absolute: the project-relative path of an absolute one inside the
    /// project (also through a link such as macOS's `/tmp` for
    /// `/private/tmp`), or the absolute paths of a relative one and its
    /// plain relative form (`./secrets/key`, `src/../vendor/lib.rs` and
    /// `build/` are `secrets/key`, `vendor/lib.rs` and `build`).
    fn path_spellings(&self, operation: &str) -> Vec<(String, bool)> {
        let Some(workspace) = &self.workspace else {
            return Vec::new();
        };
        let Some((permission, target)) = operation.split_once(':') else {
            return Vec::new();
        };
        // `~/notes.txt` is the file in the home folder, as the tools read it.
        let expanded = expand_home(target);
        let from_home = matches!(expanded, std::borrow::Cow::Owned(_));
        let target = expanded.as_ref();
        if !PATH_PERMISSIONS.contains(&permission)
            || target.is_empty()
            || target == "*"
            || target.starts_with('~')
        {
            return Vec::new();
        }
        let configured = normalize_absolute(workspace);
        let root = workspace
            .canonicalize()
            .unwrap_or_else(|_| configured.clone());
        let given = Path::new(target);
        let mut spellings = Vec::new();
        if given.is_absolute() {
            let named = normalize_absolute(given);
            if from_home {
                spellings.push((format!("{permission}:{}", named.to_string_lossy()), true));
            }
            let relative = named
                .strip_prefix(&configured)
                .or_else(|_| named.strip_prefix(&root))
                .map(Path::to_path_buf)
                .or_else(|_| {
                    resolve_permission_path(workspace, target)
                        .strip_prefix(&root)
                        .map(Path::to_path_buf)
                });
            if let Ok(relative) = relative {
                let relative = relative.to_string_lossy();
                let relative = if relative.is_empty() { "." } else { &relative };
                spellings.push((format!("{permission}:{relative}"), false));
            }
        } else {
            if let Some(plain) = plain_relative(given) {
                let plain = plain.to_string_lossy();
                let plain = if plain.is_empty() { "." } else { &plain };
                if plain != target {
                    spellings.push((format!("{permission}:{plain}"), false));
                }
            }
            for base in [&configured, &root] {
                let absolute = format!(
                    "{permission}:{}",
                    normalize_absolute(&base.join(given)).to_string_lossy()
                );
                if !spellings.iter().any(|(known, _)| known == &absolute) {
                    spellings.push((absolute, true));
                }
            }
        }
        spellings
    }

    /// Whether a tool is categorically unavailable to this agent and should
    /// therefore be omitted from the model-facing tool schema. Local/session
    /// rules are one ordered ruleset, the agent's own ceiling is checked
    /// independently, and only a final `*` deny hides the complete tool.
    /// Scoped denies remain visible because a valid argument can still be
    /// allowed at execution time. Inherited boundaries never hide a tool:
    /// sessions of one agent started from different chats then send one
    /// tool list, which keeps the cached prompt prefix they share.
    #[must_use]
    pub fn hides_tool(&self, tool: &str) -> bool {
        tool_denial(self.rules.iter().chain(&self.session_rules), tool).is_some()
            || tool_denial(self.ceilings.iter(), tool).is_some()
    }

    /// The rule of an inherited boundary (a chat above this agent) that
    /// denies the whole tool, for a refusal that says why.
    #[must_use]
    pub fn inherited_tool_denial(&self, tool: &str) -> Option<&str> {
        self.inherited
            .iter()
            .find_map(|rules| tool_denial(rules.iter(), tool))
            .map(|rule| rule.pattern.as_str())
    }

    /// Whether every call of a tool is refused: [`hides_tool`](Self::hides_tool)
    /// or an inherited boundary's whole-tool deny.
    #[must_use]
    pub fn disables_tool(&self, tool: &str) -> bool {
        self.hides_tool(tool) || self.inherited_tool_denial(tool).is_some()
    }
}

/// The rule that denies every call of `tool` in one ordered ruleset: the
/// last rule naming the tool, when it is a `*` deny.
fn tool_denial<'a>(
    rules: impl DoubleEndedIterator<Item = &'a PermissionRule>,
    tool: &str,
) -> Option<&'a PermissionRule> {
    if let Some(method) = tool.strip_prefix("browser_") {
        return ruleset_disables_browser(rules, method);
    }
    ruleset_disables_tool(rules, canonical_tool_permission(tool))
}

/// Whether `rule` covers `operation`, as written or, for a path rule, by the
/// file's other spelling: a relative rule meets the project-relative path and
/// an absolute rule the absolute one. A rule starting with `*` (the built-in
/// secret rules) or `~` matches as written, so it means what it always did.
fn rule_names(
    rule: &PermissionRule,
    operation: &str,
    spellings: &[(String, bool)],
    real_folders: &std::collections::BTreeMap<String, String>,
) -> bool {
    if wildcard_match(&rule.pattern, operation) {
        return true;
    }
    if operation.starts_with(EXTERNAL_DIRECTORY)
        && real_folders
            .get(&rule.pattern)
            .is_some_and(|real| wildcard_match(real, operation))
    {
        return true;
    }
    if spellings.is_empty() {
        return false;
    }
    let Some((_, target)) = rule.pattern.split_once(':') else {
        return false;
    };
    if target.is_empty() || target.starts_with(['*', '~']) {
        return false;
    }
    let absolute = Path::new(target).is_absolute();
    spellings.iter().any(|(spelled, spelled_absolute)| {
        *spelled_absolute == absolute && wildcard_match(&rule.pattern, spelled)
    })
}

const EXTERNAL_DIRECTORY: &str = "external_directory:";

/// Where each configured `external_directory` rule written through a folder
/// link leads, by pattern, resolved once per process: engines are built for
/// every tool call, and following a link each time would wait on a stale
/// network folder every time and follow a link someone swapped since. A
/// configuration reload ([`forget_rule_links`]) resolves them again.
static RULE_LINKS: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<String, Option<String>>>,
> = std::sync::LazyLock::new(Default::default);

/// How many times each pattern was resolved, for the tests.
#[cfg(test)]
static RULE_LINK_LOOKUPS: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<String, usize>>,
> = std::sync::LazyLock::new(Default::default);

/// [`real_folder_rule`] for a configured rule, once per process.
fn configured_real_folder(pattern: &str) -> Option<String> {
    if !pattern.starts_with(EXTERNAL_DIRECTORY) {
        return None;
    }
    if let Some(known) = RULE_LINKS.lock().get(pattern) {
        return known.clone();
    }
    // Resolved without the lock held, so a slow folder holds up no other
    // rule's check.
    let real = real_folder_rule(pattern);
    #[cfg(test)]
    {
        *RULE_LINK_LOOKUPS
            .lock()
            .entry(pattern.to_owned())
            .or_default() += 1;
    }
    RULE_LINKS
        .lock()
        .entry(pattern.to_owned())
        .or_insert(real)
        .clone()
}

/// What [`configured_real_folder`] already found for `pattern`; never looks.
fn resolved_real_folder(pattern: &str) -> Option<String> {
    RULE_LINKS.lock().get(pattern).cloned().flatten()
}

/// Forget where configured rules' links lead, for a configuration reload.
pub fn forget_rule_links() {
    RULE_LINKS.lock().clear();
}

/// An `external_directory` rule written through a folder link, such as
/// macOS's `/tmp` (really `/private/tmp`) or a linked home folder, in the
/// form the asks use: they name the folder a path really reaches. `None`
/// when the rule's folder is not absolute, does not exist or is no link.
fn real_folder_rule(pattern: &str) -> Option<String> {
    let target = pattern.strip_prefix(EXTERNAL_DIRECTORY)?;
    let literal = &target[..target.find(['*', '?']).unwrap_or(target.len())];
    let (folder, rest) = target.split_at(literal.rfind(['/', '\\'])?);
    if folder.is_empty() || !Path::new(folder).is_absolute() {
        return None;
    }
    let real = Path::new(folder).canonicalize().ok()?;
    let real = real.to_string_lossy();
    (real != folder).then(|| format!("{EXTERNAL_DIRECTORY}{real}{rest}"))
}

fn ruleset_disables_browser<'a>(
    rules: impl DoubleEndedIterator<Item = &'a PermissionRule>,
    method: &str,
) -> Option<&'a PermissionRule> {
    rules
        .rev()
        .find_map(|rule| {
            let (permission, target) = rule.pattern.split_once(':').unwrap_or((&rule.pattern, "*"));
            if !wildcard_match(permission, "browser") {
                return None;
            }
            if target == "*" {
                return Some((rule, rule.decision == PermissionDecision::Deny));
            }
            let (operation, resource) = target.split_once(':').unwrap_or((target, "*"));
            wildcard_match(operation, method).then_some((
                rule,
                resource == "*" && rule.decision == PermissionDecision::Deny,
            ))
        })
        .and_then(|(rule, denies)| denies.then_some(rule))
}

fn ruleset_disables_tool<'a>(
    rules: impl DoubleEndedIterator<Item = &'a PermissionRule>,
    tool: &str,
) -> Option<&'a PermissionRule> {
    rules
        .rev()
        .find(|rule| {
            let permission = rule
                .pattern
                .split_once(':')
                .map_or(rule.pattern.as_str(), |(p, _)| p);
            wildcard_match(permission, tool)
        })
        .filter(|rule| {
            let target = rule
                .pattern
                .split_once(':')
                .map_or("*", |(_, target)| target);
            target == "*" && rule.decision == PermissionDecision::Deny
        })
}

fn canonical_tool_permission(tool: &str) -> &str {
    match tool {
        name if name.starts_with("browser_") => "browser",
        "todo" => "todowrite",
        "write" | "apply_patch" | "patch" | "multiedit" => "edit",
        "image_generate" | "video_generate" => "media_generate",
        _ => tool,
    }
}

/// `todo` was Haarness's original write-tool spelling; `todowrite` is the
/// current Claude-compatible spelling. They are one authority boundary, so a
/// policy written with either name must govern calls made through either
/// alias. Normalize both rules and evaluated operations to prevent an alias
/// from bypassing a deny/ask decision. Upstream OpenCode names the MCP
/// resource tools `opencode_list_mcp_resources` and
/// `opencode_read_mcp_resource`, with the same patterns, and Haarness reads
/// OpenCode's configuration files: a rule under those names governs
/// `mcp_resource_list` and `mcp_resource_read`. `cloud_workspace` was called
/// `cloud_agent_session` while Cloud workspaces were called sessions, and a
/// rule saved under that name still governs it.
fn normalize_operation_alias(value: &str) -> std::borrow::Cow<'_, str> {
    for (alias, canonical) in [
        ("todo:", "todowrite:"),
        ("write:", "edit:"),
        ("apply_patch:", "edit:"),
        ("patch:", "edit:"),
        ("multiedit:", "edit:"),
        ("opencode_list_mcp_resources:", "mcp_resource_list:"),
        ("opencode_read_mcp_resource:", "mcp_resource_read:"),
        ("cloud_agent_session:", "cloud_workspace:"),
    ] {
        if let Some(suffix) = value.strip_prefix(alias) {
            return std::borrow::Cow::Owned(format!("{canonical}{suffix}"));
        }
    }
    std::borrow::Cow::Borrowed(value)
}

fn normalize_rule_aliases(mut rule: PermissionRule) -> PermissionRule {
    if let Some((permission, target)) = rule.pattern.split_once(':')
        && let Some(home) = home_folder()
    {
        let home = home.to_string_lossy().into_owned();
        let expanded = if let Some(suffix) = target.strip_prefix("~/") {
            Some(format!("{home}/{suffix}"))
        } else if target == "~" {
            Some(home)
        } else {
            target
                .strip_prefix("$HOME")
                .map(|suffix| format!("{home}{suffix}"))
        };
        if let Some(target) = expanded {
            rule.pattern = format!("{permission}:{target}");
        }
    }
    if let std::borrow::Cow::Owned(pattern) = normalize_operation_alias(&rule.pattern) {
        rule.pattern = pattern;
    }
    rule
}

/// Site approvals in the user's own Chrome carry this prefix. Their patterns
/// name the chat (`browser_site:chrome-browsing:<thread>:<origin>`), so an
/// "Allow in this chat" answer never reaches another chat.
pub const EXPLICIT_GRANT_PREFIX: &str = "browser_site:chrome-";

/// Whether a person already answered this explicit operation with an
/// "always" in this server's session. Configured rules never count: a `*` or
/// `browser_site:*` allow, project rules and custom agents can only deny it.
#[must_use]
pub fn explicit_granted(grants: &[PermissionRule], operation: &str) -> bool {
    grants
        .iter()
        .rev()
        .find(|rule| rule.pattern.starts_with(EXPLICIT_GRANT_PREFIX) && rule.pattern == operation)
        .is_some_and(|rule| rule.decision == PermissionDecision::Allow)
}

/// An explicit operation's decision: a configured or ceiling deny (or a
/// tool's deny constraint) refuses it, an answered grant allows it, and
/// anything else asks a person.
#[must_use]
pub fn explicit_decision(
    configured: PermissionDecision,
    constraint: Option<PermissionDecision>,
    granted: bool,
) -> PermissionDecision {
    if configured == PermissionDecision::Deny || constraint == Some(PermissionDecision::Deny) {
        PermissionDecision::Deny
    } else if granted {
        PermissionDecision::Allow
    } else {
        PermissionDecision::Ask
    }
}

fn rule(pattern: &str, decision: PermissionDecision) -> PermissionRule {
    PermissionRule {
        pattern: pattern.into(),
        decision,
    }
}

/// The folder a leading `~` names, for tool paths and rules alike, as shell
/// readings take it: `HOME`, else `USERPROFILE`.
fn home_folder() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(home) = TEST_HOME.with(|home| home.borrow().clone()) {
        return Some(home);
    }
    haarness_protocol::home_dir()
}

#[cfg(test)]
thread_local! {
    /// The home folder a test gives its own thread ([`TestHome`]), so a test
    /// can put a project inside a home folder without changing the
    /// environment every test shares.
    static TEST_HOME: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// While it lives, `~` on this thread names `home`.
#[cfg(test)]
pub(crate) struct TestHome;

#[cfg(test)]
impl TestHome {
    pub(crate) fn set(home: &Path) -> Self {
        TEST_HOME.with(|test| *test.borrow_mut() = Some(home.to_path_buf()));
        Self
    }
}

#[cfg(test)]
impl Drop for TestHome {
    fn drop(&mut self) {
        TEST_HOME.with(|test| *test.borrow_mut() = None);
    }
}

/// A tool path with a leading `~` read as a shell reads it: `~` alone and
/// `~/…` (and `~\…` on Windows) are in the home folder. `~name/…` and a `~`
/// anywhere else stay as written, so a project folder named `~` is written
/// `./~`. Without a home folder the path stays as written.
#[must_use]
pub fn expand_home(value: &str) -> std::borrow::Cow<'_, str> {
    let rest = if value == "~" {
        Some("")
    } else {
        value
            .strip_prefix("~/")
            .or_else(|| value.strip_prefix("~\\").filter(|_| cfg!(windows)))
    };
    match (rest, home_folder().filter(|home| home.is_absolute())) {
        (Some(""), Some(home)) => std::borrow::Cow::Owned(home.to_string_lossy().into_owned()),
        (Some(rest), Some(home)) => {
            std::borrow::Cow::Owned(home.join(rest).to_string_lossy().into_owned())
        }
        _ => std::borrow::Cow::Borrowed(value),
    }
}

/// Resolve a model-supplied tool path the same way for permission checks and
/// tool execution. Existing paths (including symlinks) are canonicalized;
/// for a new path the nearest existing ancestor is canonicalized and the
/// missing suffix is appended. This is deliberately filesystem-aware so an
/// in-workspace symlink cannot disguise an external target from the
/// `external_directory` gate.
#[must_use]
pub fn resolve_permission_path(workspace: &Path, value: &str) -> PathBuf {
    resolve_joined(&joined_path(workspace, value))
}

/// Resolve a path a tool removes rather than writes through: a deleted
/// file, or the old name of a move. The entry itself goes, not what a link
/// there leads to, so only the folder holding it is followed and the last
/// part stays as named.
#[must_use]
pub fn resolve_permission_entry(workspace: &Path, value: &str) -> PathBuf {
    let joined = joined_path(workspace, value);
    match (joined.parent(), joined.file_name()) {
        (Some(parent), Some(name)) => resolve_joined(parent).join(name),
        _ => resolve_joined(&joined),
    }
}

/// `value` on the canonical workspace, with `.` and `..` folded and a
/// leading `~` read as the home folder ([`expand_home`]).
fn joined_path(workspace: &Path, value: &str) -> PathBuf {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| normalize_absolute(workspace));
    let value = expand_home(value);
    let requested = Path::new(value.as_ref());
    normalize_absolute(&if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    })
}

fn resolve_joined(joined: &Path) -> PathBuf {
    if let Ok(path) = joined.canonicalize() {
        return path;
    }

    let mut ancestor = joined;
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            return joined.to_path_buf();
        };
        suffix.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return joined.to_path_buf();
        };
        ancestor = parent;
    }
    let mut resolved = ancestor
        .canonicalize()
        .unwrap_or_else(|_| normalize_absolute(ancestor));
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    resolved
}

/// Whether `value`, read as written (`.` and `..` folded, links not
/// followed), names a path inside `workspace`. A link inside that leads out
/// still names a path inside.
pub(crate) fn names_inside(workspace: &Path, value: &str) -> bool {
    let configured = normalize_absolute(workspace);
    let root = workspace
        .canonicalize()
        .unwrap_or_else(|_| configured.clone());
    let value = expand_home(value);
    let given = Path::new(value.as_ref());
    if given.is_absolute() {
        let named = normalize_absolute(given);
        named.starts_with(&root) || named.starts_with(&configured)
    } else {
        normalize_absolute(&root.join(given)).starts_with(&root)
    }
}

/// A relative path with `.`, `..` and trailing separators taken lexically,
/// or `None` when it climbs out of the folder it is relative to.
fn plain_relative(path: &Path) -> Option<PathBuf> {
    let mut plain = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !plain.pop() {
                    return None;
                }
            }
            Component::Normal(value) => plain.push(value),
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    Some(plain)
}

fn normalize_absolute(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    normalized
}

/// Write-denying boundary for the plan agent: only plan files may change.
#[must_use]
pub fn plan_ceiling() -> Vec<PermissionRule> {
    vec![
        // Trusted extension tools may perform arbitrary effects; never expose
        // them as a way around the planning agent's write ceiling.
        rule("ext__*:*", PermissionDecision::Deny),
        rule("browser:*", PermissionDecision::Deny),
        rule("browser:tabs_list:*", PermissionDecision::Allow),
        rule("browser:frames:*", PermissionDecision::Allow),
        rule("browser:snapshot:*", PermissionDecision::Allow),
        rule("browser:find:*", PermissionDecision::Allow),
        rule("browser:screenshot:*", PermissionDecision::Allow),
        rule("write:*", PermissionDecision::Deny),
        rule("edit:*", PermissionDecision::Deny),
        rule("apply_patch:*", PermissionDecision::Deny),
        rule("write:**/.haarness/plans/**", PermissionDecision::Allow),
        rule("write:.haarness/plans/**", PermissionDecision::Allow),
        rule("edit:**/.haarness/plans/**", PermissionDecision::Allow),
        rule("edit:.haarness/plans/**", PermissionDecision::Allow),
        rule(
            "apply_patch:**/.haarness/plans/**",
            PermissionDecision::Allow,
        ),
        rule("apply_patch:.haarness/plans/**", PermissionDecision::Allow),
        rule("media_generate:*", PermissionDecision::Deny),
        rule("cloud_agent_manage:*", PermissionDecision::Deny),
        rule("cloud_agent_manage:list:*", PermissionDecision::Allow),
        rule("cloud_agent_manage:get:*", PermissionDecision::Allow),
        rule("cloud_agent_manage:models:*", PermissionDecision::Allow),
        rule(
            "cloud_agent_manage:ssh_public_key:*",
            PermissionDecision::Allow,
        ),
        rule("cloud_schedule:*", PermissionDecision::Deny),
        rule("cloud_schedule:list:*", PermissionDecision::Allow),
        rule("cloud_schedule:get:*", PermissionDecision::Allow),
        rule("cloud_schedule:history:*", PermissionDecision::Allow),
        rule("cloud_chat:*", PermissionDecision::Deny),
        rule("cloud_chat:list:*", PermissionDecision::Allow),
        rule("cloud_chat:get:*", PermissionDecision::Allow),
        rule("cloud_chat:messages:*", PermissionDecision::Allow),
        rule("cloud_chat:wait:*", PermissionDecision::Allow),
        rule("cloud_workspace:*", PermissionDecision::Deny),
        rule("cloud_workspace:list:*", PermissionDecision::Allow),
        rule("cloud_workspace:get:*", PermissionDecision::Allow),
        rule(
            "cloud_workspace:ssh_public_key:*",
            PermissionDecision::Allow,
        ),
        rule("cloud_workspace:messages:*", PermissionDecision::Allow),
        rule("cloud_workspace:wait:*", PermissionDecision::Allow),
    ]
}

/// Wildcard matching: `*` matches anything including `/`
/// (so `**` needs no special case), `?` matches one character, matches are
/// anchored, backslashes normalize to `/`, and a pattern ending in `" *"`
/// also matches the bare form without arguments — the rule that makes
/// `bash:git status *` cover both `git status` and `git status --short`.
pub fn wildcard_match(pattern: &str, value: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, regex::Regex>>> = OnceLock::new();

    let pattern = pattern.replace('\\', "/");
    let value = value.replace('\\', "/");
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let cache = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(compiled) = cache.get(&pattern) {
            return compiled.is_match(&value);
        }
    }
    let mut escaped = String::with_capacity(pattern.len() + 8);
    for character in pattern.chars() {
        match character {
            '*' => escaped.push_str(".*"),
            '?' => escaped.push('.'),
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' => {
                escaped.push('\\');
                escaped.push(character);
            }
            other => escaped.push(other),
        }
    }
    if let Some(stripped) = escaped.strip_suffix(" .*") {
        escaped = format!("{stripped}( .*)?");
    }
    let Ok(compiled) = regex::RegexBuilder::new(&format!("^{escaped}$"))
        .dot_matches_new_line(true)
        .build()
    else {
        return false;
    };
    let matched = compiled.is_match(&value);
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() > 512 {
        cache.clear();
    }
    cache.insert(pattern, compiled);
    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_matching_rule_wins() {
        let engine = PermissionEngine::new(vec![
            rule("*", PermissionDecision::Allow),
            rule("read:**/.env", PermissionDecision::Ask),
            rule("read:fixture/.env", PermissionDecision::Deny),
        ]);
        assert_eq!(
            engine.evaluate("read:fixture/.env"),
            PermissionDecision::Deny
        );
        assert_eq!(engine.evaluate("read:other/.env"), PermissionDecision::Ask);
        assert_eq!(engine.evaluate("grep:src"), PermissionDecision::Allow);
    }

    #[test]
    fn ordinary_tool_defaults_never_prompt_for_bash_or_edits() {
        let engine = PermissionEngine::build_defaults();
        assert_eq!(
            engine.evaluate("bash:git status --short"),
            PermissionDecision::Allow
        );
        assert_eq!(engine.evaluate("bash:rm -rf /"), PermissionDecision::Allow);
        assert_eq!(
            engine.evaluate("edit:src/main.rs"),
            PermissionDecision::Allow
        );
        assert_eq!(
            engine.evaluate("write:src/main.rs"),
            PermissionDecision::Allow
        );
        assert_eq!(
            engine.evaluate("webfetch:https://example.com"),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn defaults_still_protect_secrets_and_external_paths() {
        let engine = PermissionEngine::build_defaults();
        assert_eq!(engine.evaluate("read:.env"), PermissionDecision::Ask);
        for path in [
            "service.env",
            "app/service.env.production",
            "/project/.env.local",
        ] {
            assert_eq!(
                engine.evaluate(&format!("read:{path}")),
                PermissionDecision::Ask
            );
        }
        assert_eq!(
            engine.evaluate("read:app/service.env.example"),
            PermissionDecision::Allow
        );
        assert_eq!(
            engine.evaluate("read:app/.env.production"),
            PermissionDecision::Ask
        );
        assert_eq!(
            engine.evaluate("read:app/.env.example"),
            PermissionDecision::Allow
        );
        // Packages ship `.env` fixtures: a file inside a node_modules folder
        // is read without asking, at any depth; look-alike folders still ask.
        for path in [
            "node_modules/bottleneck/.env",
            "node_modules/.env",
            "node_modules/a/node_modules/b/.env.local",
            "packages/app/node_modules/pkg/test/.env.production",
            "/project/node_modules/pkg/.env",
        ] {
            assert_eq!(
                engine.evaluate(&format!("read:{path}")),
                PermissionDecision::Allow,
                "{path}"
            );
        }
        for path in [
            "mynode_modules/.env",
            "node_modules.bak/.env",
            "src/node_modules_x/.env",
            "/project/mynode_modules/.env",
        ] {
            assert_eq!(
                engine.evaluate(&format!("read:{path}")),
                PermissionDecision::Ask,
                "{path}"
            );
        }
        // Configured rules come later and still cover them.
        let mut configured = PermissionEngine::build_defaults();
        configured.add_rule(rule("read:*.env", PermissionDecision::Deny));
        assert_eq!(
            configured.evaluate("read:node_modules/bottleneck/.env"),
            PermissionDecision::Deny
        );
        assert_eq!(
            engine.evaluate("external_directory:/tmp/*"),
            PermissionDecision::Ask
        );
        assert_eq!(engine.evaluate("doom_loop:*"), PermissionDecision::Ask);
        assert_eq!(
            engine.evaluate("agent_switch:build"),
            PermissionDecision::Ask
        );
        assert_eq!(
            engine.evaluate("media_generate:image:generate"),
            PermissionDecision::Ask
        );
    }

    #[test]
    fn secret_paths_are_exactly_the_built_in_read_asks() {
        let engine = PermissionEngine::build_defaults();
        for path in [
            ".env",
            "prod.env",
            "app.env.local",
            "conf.env.d/notes.txt",
            "/project/.env.production",
            "c.env.example",
            "d.txt",
            "README.md",
            ".env/bin/activate",
            ".env/node_modules",
            "node_modules/pkg/.env",
            "/project/a/node_modules/pkg/.env.local",
            "mynode_modules/.env",
        ] {
            assert_eq!(
                is_secret_path(Path::new(path)),
                engine.evaluate(&format!("read:{path}")) == PermissionDecision::Ask,
                "{path}"
            );
        }
        // `..` is resolved first, so a path that only passes through
        // node_modules is judged by the file it reaches.
        for path in [
            "node_modules/../.env",
            "node_modules/pkg/../../.env",
            "/project/node_modules/x/../../.env",
        ] {
            assert!(is_secret_path(Path::new(path)), "{path}");
        }
        assert!(!is_secret_path(Path::new("conf.env.d/../src/main.rs")));
    }

    #[test]
    fn cloud_plan_reads_cannot_escalate_to_remote_execution() {
        let mut engine = PermissionEngine::plan_defaults();
        engine.approve_always("cloud_workspace:*");
        engine.add_rule(rule("cloud_agent_manage:*", PermissionDecision::Allow));
        for action in ["list", "get", "messages", "wait"] {
            assert_eq!(
                engine.evaluate(&format!("cloud_workspace:{action}:workspace-id")),
                PermissionDecision::Allow
            );
        }
        for action in [
            "create", "start", "ask", "stop", "abort", "rename", "delete",
        ] {
            assert_eq!(
                engine.evaluate(&format!("cloud_workspace:{action}:workspace-id")),
                PermissionDecision::Deny
            );
        }
        for action in ["list", "get", "ssh_public_key", "models"] {
            assert_eq!(
                engine.evaluate(&format!("cloud_agent_manage:{action}:agent-id")),
                PermissionDecision::Allow
            );
        }
        for action in ["create", "update", "delete"] {
            assert_eq!(
                engine.evaluate(&format!("cloud_agent_manage:{action}:agent-id")),
                PermissionDecision::Deny
            );
        }
        for (tool, reads, writes) in [
            (
                "cloud_chat",
                vec!["list", "get", "messages", "wait"],
                vec!["create", "rename", "archive", "ask", "abort"],
            ),
            (
                "cloud_schedule",
                vec!["list", "get", "history"],
                vec!["create", "update", "pause", "resume", "run", "delete"],
            ),
        ] {
            engine.approve_always(format!("{tool}:*"));
            for action in reads {
                assert_eq!(
                    engine.evaluate(&format!("{tool}:{action}:id")),
                    PermissionDecision::Allow
                );
            }
            for action in writes {
                assert_eq!(
                    engine.evaluate(&format!("{tool}:{action}:id")),
                    PermissionDecision::Deny
                );
            }
        }
        let mut build = PermissionEngine::build_defaults();
        build.add_rule(rule("cloud_workspace:*", PermissionDecision::Deny));
        assert_eq!(
            build.evaluate("cloud_workspace:ask:workspace-id"),
            PermissionDecision::Deny
        );
        // A rule saved under the tool's former name still governs it.
        let legacy = PermissionEngine::new(vec![
            rule("*", PermissionDecision::Allow),
            rule("cloud_agent_session:delete:*", PermissionDecision::Deny),
        ]);
        assert_eq!(
            legacy.evaluate("cloud_workspace:delete:workspace-id"),
            PermissionDecision::Deny
        );
    }

    #[test]
    fn plan_ceiling_denies_writes_and_cannot_be_reopened() {
        let mut engine = PermissionEngine::plan_defaults();
        // Even an explicit user allow cannot pierce the ceiling.
        engine.add_rule(rule("write:*", PermissionDecision::Allow));
        engine.approve_always("edit:src/main.rs");
        assert_eq!(
            engine.evaluate("write:src/main.rs"),
            PermissionDecision::Deny
        );
        assert_eq!(
            engine.evaluate("edit:src/main.rs"),
            PermissionDecision::Deny
        );
        assert_eq!(
            engine.evaluate("write:.haarness/plans/next.md"),
            PermissionDecision::Allow
        );
        assert_eq!(engine.evaluate("bash:ls"), PermissionDecision::Allow);
        assert_eq!(
            engine.evaluate("media_generate:image:generate"),
            PermissionDecision::Deny
        );
    }

    #[test]
    fn a_ceiling_holds_whatever_boundary_comes_after_it() {
        // The plan agent running below a build chat: the parent's boundary
        // ends with its `*` allow, which must not reopen the plan ceiling.
        let mut child = PermissionEngine::plan_defaults();
        child.add_inherited_ceiling(PermissionEngine::build_defaults().boundary());
        assert_eq!(child.evaluate("edit:src/main.rs"), PermissionDecision::Deny);
        assert!(child.disables_tool("media_generate"));
        // A workflow under it inherits both boundaries, nearest first.
        let mut workflow = PermissionEngine::build_defaults();
        workflow.add_inherited_ceiling(child.boundary());
        workflow.add_inherited_ceiling(PermissionEngine::build_defaults().boundary());
        assert_eq!(
            workflow.evaluate("write:src/main.rs"),
            PermissionDecision::Deny
        );
        assert_eq!(
            workflow.evaluate("write:.haarness/plans/next.md"),
            PermissionDecision::Allow
        );
        assert_eq!(workflow.evaluate("bash:ls"), PermissionDecision::Allow);
    }

    #[test]
    fn an_inherited_whole_tool_deny_refuses_the_tool_but_keeps_it_listed() {
        let mut parent = PermissionEngine::build_defaults();
        parent.add_rule(rule("webfetch:*", PermissionDecision::Deny));
        parent.add_rule(rule("browser:*", PermissionDecision::Deny));
        let mut child = PermissionEngine::build_defaults();
        child.add_inherited_ceiling(parent.boundary());
        for tool in ["webfetch", "browser_navigate"] {
            assert!(!child.hides_tool(tool), "{tool}");
            assert!(child.disables_tool(tool), "{tool}");
        }
        assert_eq!(child.inherited_tool_denial("webfetch"), Some("webfetch:*"));
        assert_eq!(
            child.inherited_tool_denial("browser_navigate"),
            Some("browser:*")
        );
        assert_eq!(child.inherited_tool_denial("read"), None);
        // The agent's own ceiling and rules still take a tool out.
        let mut plan = PermissionEngine::plan_defaults();
        plan.add_rule(rule("task:*", PermissionDecision::Deny));
        assert!(plan.hides_tool("media_generate"));
        assert!(plan.hides_tool("task"));
        assert_eq!(plan.inherited_tool_denial("media_generate"), None);
    }

    #[test]
    fn only_categorical_denies_hide_tools_from_the_model() {
        let mut engine = PermissionEngine::build_defaults();
        engine.add_rule(rule("task:*", PermissionDecision::Deny));
        engine.add_rule(rule("read:private/*", PermissionDecision::Deny));
        assert!(engine.disables_tool("task"));
        assert!(
            !engine.disables_tool("read"),
            "a scoped deny still leaves valid read arguments"
        );
        assert!(!engine.disables_tool("bash"));

        let plan = PermissionEngine::plan_defaults();
        assert!(
            !plan.disables_tool("write"),
            "the later plan-directory exception keeps write usable"
        );

        let mut ceiling = PermissionEngine::build_defaults();
        ceiling.add_ceiling(rule("webfetch:*", PermissionDecision::Deny));
        assert!(ceiling.disables_tool("webfetch"));
    }

    #[test]
    fn trailing_space_star_matches_with_and_without_arguments() {
        assert!(wildcard_match("bash:git status *", "bash:git status"));
        assert!(wildcard_match(
            "bash:git status *",
            "bash:git status --short"
        ));
        assert!(!wildcard_match("bash:git status *", "bash:git stash"));
        assert!(wildcard_match("bash:ls *", "bash:ls"));
        assert!(wildcard_match("bash:ls *", "bash:ls -la"));
    }

    #[test]
    fn wildcard_semantics_match_haarness() {
        // `*` crosses path separators (dotall) — `**` needs no special case.
        assert!(wildcard_match("read:*/.env", "read:deep/nested/.env"));
        assert!(wildcard_match("read:**/.env", "read:deep/nested/.env"));
        // `?` matches exactly one character.
        assert!(wildcard_match("bash:l?", "bash:ls"));
        assert!(!wildcard_match("bash:l?", "bash:l"));
        // Anchored on both ends.
        assert!(!wildcard_match("bash:git", "bash:git status"));
        // Regex specials in patterns are literals.
        assert!(wildcard_match("bash:echo (hi) [ok]", "bash:echo (hi) [ok]"));
        // Backslashes normalize so Windows-style paths still match.
        assert!(wildcard_match("read:src/*", "read:src\\main.rs"));
        // `*` also spans newlines (multi-line commands stay one operation).
        assert!(wildcard_match("bash:echo *", "bash:echo 'a\nb'"));
    }

    #[test]
    fn config_rules_override_defaults_with_last_match_wins() {
        let mut engine = PermissionEngine::build_defaults();
        engine.add_rule(rule("bash:*", PermissionDecision::Ask));
        engine.add_rule(rule("bash:git status *", PermissionDecision::Allow));

        assert_eq!(
            engine.evaluate("bash:git status --short"),
            PermissionDecision::Allow
        );
        assert_eq!(engine.evaluate("bash:git push"), PermissionDecision::Ask);
    }

    #[test]
    fn todo_write_aliases_share_one_permission_boundary() {
        let canonical_deny = PermissionEngine::new(vec![
            rule("*", PermissionDecision::Allow),
            rule("todowrite:*", PermissionDecision::Deny),
        ]);
        assert_eq!(
            canonical_deny.evaluate("todo:*"),
            PermissionDecision::Deny,
            "legacy invocation cannot bypass canonical policy"
        );

        let legacy_ask = PermissionEngine::new(vec![
            rule("*", PermissionDecision::Allow),
            rule("todo:*", PermissionDecision::Ask),
        ]);
        assert_eq!(
            legacy_ask.evaluate("todowrite:*"),
            PermissionDecision::Ask,
            "canonical invocation cannot bypass legacy policy"
        );
        assert_eq!(
            legacy_ask.flattened_rules().last().unwrap().pattern,
            "todowrite:*",
            "persisted boundaries use the canonical spelling"
        );
    }

    #[test]
    fn upstream_resource_tool_names_govern_the_resource_tools() {
        let mut engine = PermissionEngine::build_defaults();
        for (pattern, decision) in [
            ("opencode_read_mcp_resource:*", PermissionDecision::Deny),
            (
                "opencode_read_mcp_resource:docs:*",
                PermissionDecision::Allow,
            ),
            ("opencode_list_mcp_resources:wiki", PermissionDecision::Ask),
        ] {
            engine.add_rule(rule(pattern, decision));
        }
        assert_eq!(
            engine.evaluate("mcp_resource_read:wiki:wiki://page"),
            PermissionDecision::Deny
        );
        // In its own place in the rule order: the later allow decides.
        assert_eq!(
            engine.evaluate("mcp_resource_read:docs:docs://setup"),
            PermissionDecision::Allow
        );
        assert_eq!(
            engine.evaluate("mcp_resource_list:wiki"),
            PermissionDecision::Ask
        );
        assert_eq!(
            engine.evaluate("mcp_resource_list:docs"),
            PermissionDecision::Allow
        );
        let mut whole = PermissionEngine::build_defaults();
        whole.add_rule(rule(
            "opencode_read_mcp_resource:*",
            PermissionDecision::Deny,
        ));
        assert!(whole.disables_tool("mcp_resource_read"));
        assert!(!whole.disables_tool("mcp_resource_list"));
    }

    #[test]
    fn edit_tool_aliases_share_one_permission_boundary() {
        let mut engine = PermissionEngine::build_defaults();
        engine.add_rule(rule("edit:*", PermissionDecision::Deny));
        assert_eq!(
            engine.evaluate("write:src/lib.rs"),
            PermissionDecision::Deny
        );
        assert_eq!(
            engine.evaluate("apply_patch:src/lib.rs"),
            PermissionDecision::Deny
        );
        assert!(engine.disables_tool("write"));
        assert!(engine.disables_tool("apply_patch"));
    }

    #[cfg(unix)]
    #[test]
    fn an_outside_folder_rule_written_through_a_link_meets_the_ask_for_where_it_leads() {
        let root =
            std::env::temp_dir().join(format!("haarness-linked-rules-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(root.join("real/sub")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let link = root.join("link").to_string_lossy().into_owned();
        let real = root.join("real").canonicalize().unwrap();
        let real = real.to_string_lossy();
        let engine = PermissionEngine::new(vec![
            rule(
                &format!("external_directory:{link}/*"),
                PermissionDecision::Allow,
            ),
            rule(
                &format!("external_directory:{link}/sub/*"),
                PermissionDecision::Deny,
            ),
        ]);
        assert_eq!(
            engine.evaluate(&format!("external_directory:{real}/*")),
            PermissionDecision::Allow
        );
        assert_eq!(
            engine.evaluate(&format!("external_directory:{real}/sub/*")),
            PermissionDecision::Deny
        );
        assert_eq!(
            engine.evaluate(&format!("external_directory:{real}-other/*")),
            PermissionDecision::Ask
        );
        // The link is followed when the rules are loaded, never on a check:
        // pointing it elsewhere later does not carry the rule along.
        std::fs::create_dir_all(root.join("elsewhere")).unwrap();
        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("link")).unwrap();
        let elsewhere = root.join("elsewhere").canonicalize().unwrap();
        assert_eq!(
            engine.evaluate(&format!("external_directory:{}/*", elsewhere.display())),
            PermissionDecision::Ask
        );
        assert_eq!(
            engine.evaluate(&format!("external_directory:{real}/*")),
            PermissionDecision::Allow
        );
        // macOS's /tmp is /private/tmp, the folder an ask names.
        let tmp = Path::new("/tmp").canonicalize().unwrap();
        if tmp != Path::new("/tmp") {
            let engine = PermissionEngine::new(vec![rule(
                "external_directory:/tmp/*",
                PermissionDecision::Allow,
            )]);
            assert_eq!(
                engine.evaluate(&format!("external_directory:{}/build/*", tmp.display())),
                PermissionDecision::Allow
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_configured_rule_s_link_is_followed_once_per_process_and_an_answer_s_never() {
        let root =
            std::env::temp_dir().join(format!("haarness-rule-links-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(root.join("first")).unwrap();
        std::fs::create_dir_all(root.join("second")).unwrap();
        std::os::unix::fs::symlink(root.join("first"), root.join("link")).unwrap();
        let first = root.join("first").canonicalize().unwrap();
        let second = root.join("second").canonicalize().unwrap();
        let pattern = format!("external_directory:{}/*", root.join("link").display());
        let ask = |folder: &Path| format!("external_directory:{}/*", folder.display());
        // Engines are built afresh for every tool call.
        let fresh = || PermissionEngine::new(vec![rule(&pattern, PermissionDecision::Allow)]);
        assert_eq!(fresh().evaluate(&ask(&first)), PermissionDecision::Allow);
        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink(root.join("second"), root.join("link")).unwrap();
        let later = fresh();
        assert_eq!(later.evaluate(&ask(&first)), PermissionDecision::Allow);
        assert_eq!(later.evaluate(&ask(&second)), PermissionDecision::Ask);
        assert_eq!(
            RULE_LINK_LOOKUPS.lock()[&pattern],
            1,
            "a cache hit looks nothing up"
        );
        // An Always answer, a repository grant and a parent's boundary are
        // never followed: they name the folder their ask named.
        let answer = format!("external_directory:{}/*", root.join("link").display());
        let mut engine = PermissionEngine::new(Vec::new());
        engine.add_grant(rule(&format!("{answer}x"), PermissionDecision::Allow));
        engine.add_repository_grant(rule(&format!("{answer}y"), PermissionDecision::Allow));
        engine.add_inherited_ceiling([vec![rule(&format!("{answer}z"), PermissionDecision::Deny)]]);
        engine.approve_always(format!("{answer}w"));
        for suffix in ["x", "y", "z", "w"] {
            let pattern = format!("{answer}{suffix}");
            assert!(!RULE_LINK_LOOKUPS.lock().contains_key(&pattern), "{suffix}");
            assert_eq!(
                engine.evaluate(&format!(
                    "external_directory:{}/*{suffix}",
                    second.display()
                )),
                PermissionDecision::Ask,
                "{suffix}"
            );
        }
        // A parent's configured rule reaches its child as it was resolved.
        let mut child = PermissionEngine::new(Vec::new());
        child.add_inherited_ceiling([vec![rule(&pattern, PermissionDecision::Deny)]]);
        assert_eq!(child.evaluate(&ask(&first)), PermissionDecision::Deny);
        assert_eq!(RULE_LINK_LOOKUPS.lock()[&pattern], 1);
        // A configuration reload follows the link again.
        forget_rule_links();
        assert_eq!(fresh().evaluate(&ask(&second)), PermissionDecision::Allow);
        assert_eq!(RULE_LINK_LOOKUPS.lock()[&pattern], 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configured_home_patterns_expand_like_haarness() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let engine = PermissionEngine::new(vec![rule(
            "external_directory:~/.cache/demo/*",
            PermissionDecision::Allow,
        )]);
        let operation = format!(
            "external_directory:{}/.cache/demo/*",
            PathBuf::from(home).display()
        );
        assert_eq!(engine.evaluate(&operation), PermissionDecision::Allow);
    }

    #[test]
    fn an_always_answer_never_reopens_a_configured_deny() {
        let mut engine = PermissionEngine::new(vec![
            rule("bash:*", PermissionDecision::Ask),
            rule("bash:git push --force *", PermissionDecision::Deny),
        ]);
        // "Always" on `git push origin main` learns `git push *`.
        engine.add_grant(rule("bash:git push *", PermissionDecision::Allow));
        assert_eq!(
            engine.evaluate("bash:git push origin main"),
            PermissionDecision::Allow
        );
        assert_eq!(
            engine.evaluate("bash:git push --force origin main"),
            PermissionDecision::Deny
        );
        // A child agent's boundary keeps the deny too.
        let mut child = PermissionEngine::new(vec![rule("bash:*", PermissionDecision::Allow)]);
        child.add_inherited_ceiling(engine.boundary());
        assert_eq!(
            child.evaluate("bash:git push --force origin main"),
            PermissionDecision::Deny
        );
    }

    #[test]
    fn always_prefix_rule_covers_future_variants() {
        let mut engine = PermissionEngine::new(vec![rule("bash:*", PermissionDecision::Ask)]);
        // "Allow always" on `cargo test -p foo` learns the arity prefix.
        engine.approve_always("bash:cargo test *");
        assert_eq!(
            engine.evaluate("bash:cargo test"),
            PermissionDecision::Allow
        );
        assert_eq!(
            engine.evaluate("bash:cargo test -p foo"),
            PermissionDecision::Allow
        );
        assert_eq!(engine.evaluate("bash:cargo run"), PermissionDecision::Ask);
    }

    #[test]
    fn a_leading_tilde_in_a_tool_path_is_the_home_folder() {
        let home = home_folder().expect("tests run with a home folder");
        assert_eq!(expand_home("~"), home.to_string_lossy());
        assert_eq!(
            expand_home("~/notes.txt"),
            home.join("notes.txt").to_string_lossy()
        );
        for kept in ["~user/x", "a/~/b", "./~/x", "~x", "notes~", ""] {
            assert_eq!(expand_home(kept), kept);
        }
        #[cfg(not(windows))]
        assert_eq!(expand_home("~\\x"), "~\\x");

        let workspace =
            std::env::temp_dir().join(format!("haarness-tilde-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(workspace.join("~")).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let real_home = home.canonicalize().unwrap_or_else(|_| home.clone());
        // Never a folder named `~` inside the project.
        assert_eq!(
            resolve_permission_path(&workspace, "~/haarness-no-such-file"),
            real_home.join("haarness-no-such-file")
        );
        assert_eq!(resolve_permission_path(&workspace, "~"), real_home);
        assert_eq!(
            resolve_permission_entry(&workspace, "~/link"),
            real_home.join("link")
        );
        assert_eq!(
            resolve_permission_path(&workspace, "./~/x"),
            workspace.join("~/x")
        );
        assert!(!names_inside(&workspace, "~/x"));
        assert!(names_inside(&workspace, "./~/x"));

        // A rule for a home folder meets a tool path written with `~`, and
        // a project file written through it meets the project's rules.
        let mut engine = PermissionEngine::build_defaults();
        engine.add_rule(rule("read:~/.ssh/*", PermissionDecision::Deny));
        engine.add_rule(rule("read:secrets/*", PermissionDecision::Deny));
        engine.set_workspace(&workspace);
        assert_eq!(
            engine.evaluate("read:~/.ssh/id_rsa"),
            PermissionDecision::Deny
        );
        assert_eq!(
            engine.evaluate("read:./~/.ssh/id_rsa"),
            PermissionDecision::Allow
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }

    /// A project in the home folder, as most are: `~/project/…` is a
    /// project file for its rules, and Haarness's own configuration in the
    /// home folder asks for an edit written with `~` too.
    #[test]
    fn a_project_file_written_from_the_home_folder_meets_the_project_rules() {
        let root = std::env::temp_dir()
            .join(format!("haarness-tilde-home-{}", uuid::Uuid::now_v7()))
            .join("home");
        let workspace = root.join("project");
        std::fs::create_dir_all(workspace.join("secrets")).unwrap();
        let home = root.canonicalize().unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let guard = TestHome::set(&home);
        assert_eq!(
            resolve_permission_path(&workspace, "~/project/secrets/key"),
            workspace.join("secrets/key")
        );
        assert!(names_inside(&workspace, "~/project/secrets/key"));
        let mut engine = PermissionEngine::build_defaults();
        engine.add_rule(rule("read:secrets/*", PermissionDecision::Deny));
        engine.add_rule(rule("edit:~/notes/*", PermissionDecision::Deny));
        engine.set_workspace(&workspace);
        for operation in ["read:~/project/secrets/key", "read:secrets/key"] {
            assert_eq!(
                engine.evaluate(operation),
                PermissionDecision::Deny,
                "{operation}"
            );
        }
        assert_eq!(
            engine.evaluate("edit:~/notes/todo.md"),
            PermissionDecision::Deny
        );
        assert_eq!(
            engine.evaluate(&format!("edit:{}/notes/todo.md", home.display())),
            PermissionDecision::Deny
        );
        // The global configuration folder, named from a home folder that
        // holds it.
        if let Some(global) = global_config_dir()
            && let (Some(parent), Some(name)) = (global.parent(), global.file_name())
        {
            drop(guard);
            let _home = TestHome::set(parent);
            let edit = format!("edit:~/{}/config.toml", name.to_string_lossy());
            let evaluation = engine.evaluate_detailed(&edit);
            assert_eq!(evaluation.decision, PermissionDecision::Ask, "{edit}");
            assert!(evaluation.protected, "{edit}");
        }
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn path_rules_name_a_project_file_by_either_spelling() {
        let project = Path::new("/work/project");
        let mut engine = PermissionEngine::build_defaults();
        engine.add_rule(rule("read:secrets/*", PermissionDecision::Deny));
        engine.add_rule(rule(
            "edit:/work/project/vendor/*",
            PermissionDecision::Deny,
        ));
        engine.add_rule(rule("list:build", PermissionDecision::Ask));
        // Without the project folder, rules match the path as written.
        assert_eq!(
            engine.evaluate("read:/work/project/secrets/key"),
            PermissionDecision::Allow
        );
        engine.set_workspace(project);
        for operation in [
            "read:secrets/key",
            "read:/work/project/secrets/key",
            "read:/work/project/./src/../secrets/key",
            "edit:vendor/lib.rs",
            "write:vendor/lib.rs",
            "edit:/work/project/vendor/lib.rs",
        ] {
            assert_eq!(
                engine.evaluate(operation),
                PermissionDecision::Deny,
                "{operation}"
            );
        }
        assert_eq!(
            engine.evaluate("list:/work/project/build"),
            PermissionDecision::Ask
        );
        // `./`, `..` inside the project and a trailing separator are the
        // same file or folder.
        for operation in [
            "read:./secrets/key",
            "read:src/../secrets/key",
            "edit:./vendor/lib.rs",
            "write:src/../vendor/lib.rs",
        ] {
            assert_eq!(
                engine.evaluate(operation),
                PermissionDecision::Deny,
                "{operation}"
            );
        }
        assert_eq!(engine.evaluate("list:build/"), PermissionDecision::Ask);
        assert_eq!(engine.evaluate("list:./build"), PermissionDecision::Ask);
        // The deny names the rule as written.
        assert_eq!(
            engine
                .evaluate_detailed("read:/work/project/secrets/key")
                .matched
                .as_deref(),
            Some("read:secrets/*")
        );
        // Outside the project, or another permission, nothing changes.
        for operation in [
            "read:/elsewhere/secrets/key",
            "read:../secrets/key",
            "bash:cat /work/project/secrets/key",
            "read:/work/project-other/secrets/key",
        ] {
            assert_eq!(
                engine.evaluate(operation),
                PermissionDecision::Allow,
                "{operation}"
            );
        }
    }

    #[test]
    fn rules_starting_with_a_star_still_match_only_as_written() {
        // A project inside a node_modules folder: a relative `.env` read asks
        // as before, although its absolute path is inside node_modules.
        let mut engine = PermissionEngine::build_defaults();
        engine.set_workspace(Path::new("/work/node_modules/project"));
        assert_eq!(engine.evaluate("read:.env"), PermissionDecision::Ask);
        assert_eq!(
            engine.evaluate("read:/work/node_modules/project/.env"),
            PermissionDecision::Allow
        );
        // Ceilings and grants follow the same matching as configured rules.
        let mut child = PermissionEngine::build_defaults();
        child.set_workspace(Path::new("/work/project"));
        child.add_ceiling(rule("edit:generated/*", PermissionDecision::Deny));
        child.approve_always("read:/work/project/notes/.env");
        assert_eq!(
            child.evaluate("edit:/work/project/generated/a.rs"),
            PermissionDecision::Deny
        );
        assert_eq!(child.evaluate("read:notes/.env"), PermissionDecision::Allow);
    }

    #[test]
    fn a_project_path_through_a_folder_link_is_still_a_project_path() {
        // macOS keeps temporary folders under /private/var, also reachable
        // as /var: a rule for the project meets a path through either.
        let project = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("haarness-path-rules-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(project.join("secrets")).unwrap();
        let mut engine = PermissionEngine::build_defaults();
        engine.add_rule(rule("read:secrets/*", PermissionDecision::Deny));
        engine.set_workspace(&project);
        let canonical = project.join("secrets/key").to_string_lossy().into_owned();
        assert_eq!(
            engine.evaluate(&format!("read:{canonical}")),
            PermissionDecision::Deny
        );
        if let Some(alias) = canonical.strip_prefix("/private") {
            assert_eq!(
                engine.evaluate(&format!("read:{alias}")),
                PermissionDecision::Deny,
                "{alias}"
            );
        }
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn configuration_files_are_named_at_any_depth_and_nothing_else_is() {
        for path in [
            "haarness.json",
            "haarness.jsonc",
            ".haarness/haarness.json",
            ".haarness/cloud/haarness.json",
            "packages/app/opencode.jsonc",
            ".opencode/opencode.json",
            ".haarness/config.toml",
            ".claude/settings.json",
            "/work/project/.claude/settings.local.json",
            ".mcp.json",
            "./src/../Haarness.JSON",
            // Windows drops trailing dots and spaces, so these are the files.
            ".mcp.json.",
            "haarness.json ",
            ".claude./settings.json. .",
            // A project plugin's hooks and MCP servers, and extensions.
            ".haarness/plugins/kit/hooks/hooks.json",
            ".haarness/plugins/kit/.claude-plugin/plugin.json",
            ".haarness/extensions.json",
            ".haarness/extensions/approve.js",
            ".haarness/extensions/pkg/dist/index.mjs",
        ] {
            assert!(is_config_path(Path::new(path)), "{path}");
        }
        // The global configuration folder's own settings and extensions;
        // the CLI's layout resolution is pinned by its own tests.
        let global = Path::new("/home/me/.config/haarness");
        for name in [
            "config.toml",
            "extensions.json",
            "extensions/x.js",
            "haarness.json",
        ] {
            assert!(is_global_config_path(&global.join(name), global), "{name}");
        }
        for name in ["AGENTS.md", "commands/x.md", "extensions", "prompts"] {
            assert!(!is_global_config_path(&global.join(name), global), "{name}");
        }
        // Its prompt files: an agent must not rewrite its own instructions.
        for name in ["prompts/system.md", "Prompts/Plan.md", "prompts/new.md"] {
            assert!(is_global_config_path(&global.join(name), global), "{name}");
        }
        // Every file of the system folder, at any depth.
        let base = Path::new("/opt/team/haarness");
        for name in [
            "haarness.json",
            "prompts/system.md",
            "agents/reviewer.md",
            "skills/check/SKILL.md",
            "plugins/local/kit/hooks/hooks.json",
            "AGENTS.md",
            "new-file.txt",
        ] {
            assert!(is_system_config_path(&base.join(name), base), "{name}");
        }
        assert!(is_system_config_path(
            Path::new("/opt/team/haarness/x/../y.md"),
            base
        ));
        for path in [
            "/opt/team/haarness",
            "/opt/team/other.md",
            "/opt/team/haarness-2/x.md",
        ] {
            assert!(!is_system_config_path(Path::new(path), base), "{path}");
        }
        for path in [
            "config.toml",
            "src/config.toml",
            "settings.json",
            ".vscode/settings.json",
            "my-haarness.json",
            "haarness.json.bak",
            "package.json",
            "hooks.json",
            "src/hooks/hooks.ts",
            "plugin.json",
            "extensions/x.js",
            "",
        ] {
            assert!(!is_config_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn an_edit_in_the_system_folder_always_asks_a_person() {
        // The only test that records a system folder: a path nothing else edits.
        let base = std::env::temp_dir().join("haarness-permission-base-fixture");
        set_system_config_dir(Some(base.clone()));
        let mut engine = PermissionEngine::build_defaults();
        engine.add_rule(rule("edit:*", PermissionDecision::Allow));
        engine.add_rule(rule("*", PermissionDecision::Allow));
        engine.approve_always("edit:*");
        for file in ["prompts/system.md", "agents/reviewer.md", "notes.txt"] {
            let operation = format!("edit:{}", base.join(file).display());
            let evaluation = engine.evaluate_detailed(&operation);
            assert_eq!(evaluation.decision, PermissionDecision::Ask, "{operation}");
            assert!(evaluation.protected, "{operation}");
            assert_eq!(evaluation.matched.as_deref(), Some(CONFIG_EDIT_RULE));
        }
        let beside = format!("edit:{}-other/notes.txt", base.display());
        assert_eq!(engine.evaluate(&beside), PermissionDecision::Allow);
    }

    #[test]
    fn a_configuration_file_edit_asks_a_person_unless_a_rule_names_it() {
        let asks = |engine: &PermissionEngine, operation: &str| {
            let evaluation = engine.evaluate_detailed(operation);
            assert_eq!(evaluation.decision, PermissionDecision::Ask, "{operation}");
            assert!(evaluation.protected, "{operation}");
            assert_eq!(evaluation.matched.as_deref(), Some(CONFIG_EDIT_RULE));
        };
        let mut engine = PermissionEngine::build_defaults();
        for operation in [
            "edit:haarness.json",
            "edit:sub/.haarness/haarness.json",
            "edit:/work/project/opencode.jsonc",
            "edit:.haarness/config.toml",
            "edit:./x/../.mcp.json",
        ] {
            asks(&engine, operation);
        }
        // Other files, and reading a configuration file, are unchanged.
        assert_eq!(
            engine.evaluate("edit:src/main.rs"),
            PermissionDecision::Allow
        );
        assert_eq!(
            engine.evaluate("read:haarness.json"),
            PermissionDecision::Allow
        );
        assert!(!engine.evaluate_detailed("edit:src/main.rs").protected);
        // A blanket allow and an "always" answer to another edit leave it asking.
        engine.add_rule(rule("edit:*", PermissionDecision::Allow));
        engine.add_rule(rule("*", PermissionDecision::Allow));
        engine.add_rule(rule("edit:**/*", PermissionDecision::Allow));
        engine.approve_always("edit:*");
        asks(&engine, "edit:haarness.json");
        // A rule naming the file opens it; a deny refuses it.
        engine.add_rule(rule("edit:haarness.json", PermissionDecision::Allow));
        let allowed = engine.evaluate_detailed("edit:haarness.json");
        assert_eq!(allowed.decision, PermissionDecision::Allow);
        assert!(!allowed.protected);
        asks(&engine, "edit:.haarness/haarness.json");
        // An allow for a folder or a file kind does not name the file.
        for broad in [
            "edit:.claude/**",
            "edit:.haarness/**",
            "edit:*.json",
            "edit:*.toml",
        ] {
            engine.add_rule(rule(broad, PermissionDecision::Allow));
        }
        for operation in [
            "edit:.claude/settings.json",
            "edit:.haarness/config.toml",
            "edit:.haarness/haarness.json",
        ] {
            asks(&engine, operation);
        }
        engine.add_rule(rule("edit:*.mcp.json", PermissionDecision::Allow));
        assert_eq!(engine.evaluate("edit:.mcp.json"), PermissionDecision::Allow);
        engine.add_rule(rule("edit:**/haarness.json", PermissionDecision::Allow));
        assert_eq!(
            engine.evaluate("edit:pkg/haarness.json"),
            PermissionDecision::Allow
        );
        // A named allow still counts when a blanket allow comes after it.
        let mut later = PermissionEngine::build_defaults();
        later.add_rule(rule("edit:opencode.json", PermissionDecision::Allow));
        later.add_rule(rule("edit:*", PermissionDecision::Allow));
        assert_eq!(
            later.evaluate("edit:opencode.json"),
            PermissionDecision::Allow
        );
        engine.add_rule(rule("edit:opencode.json", PermissionDecision::Deny));
        let denied = engine.evaluate_detailed("edit:opencode.json");
        assert_eq!(denied.decision, PermissionDecision::Deny);
        assert!(!denied.protected);
        // The plan agent's write ceiling still refuses it outright.
        assert_eq!(
            PermissionEngine::plan_defaults().evaluate("edit:haarness.json"),
            PermissionDecision::Deny
        );
    }
}
