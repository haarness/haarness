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
/// and `.claude-plugin/plugin.json`, and the global configuration folder's
/// `config.toml`, `extensions.json` and `extensions/`. An agent that changes
/// one could widen its own permissions at the next reload, so an edit asks a
/// person unless a rule naming the file allows it. Names compare without
/// case and without the trailing dots and spaces Windows drops.
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
    global_config_dir().is_some_and(|global| {
        let global = normalize_absolute(&global);
        let real = global.canonicalize().unwrap_or_else(|_| global.clone());
        [global, real].iter().any(|root| {
            path.strip_prefix(root).is_ok_and(|rest| {
                let rest: Vec<String> = rest
                    .components()
                    .map(|part| file_name_key(&part.as_os_str().to_string_lossy()))
                    .collect();
                matches!(
                    rest.iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .as_slice(),
                    ["config.toml" | "extensions.json"] | ["extensions", _, ..]
                )
            })
        })
    })
}

/// A file or folder name as Haarness compares it: lower case, without the
/// trailing dots and spaces Windows drops when it creates the file.
fn file_name_key(name: &str) -> String {
    name.trim_end_matches(['.', ' ']).to_ascii_lowercase()
}

/// The global configuration folder, as the CLI's layout finds it:
/// `$HAARNESS_HOME/config`, else `$XDG_CONFIG_HOME/haarness`, else
/// `~/.config/haarness`.
fn global_config_dir() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(root) = absolute("HAARNESS_HOME") {
        return Some(root.join("config"));
    }
    absolute("XDG_CONFIG_HOME")
        .or_else(|| absolute("HOME").map(|home| home.join(".config")))
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
        Self {
            rules: rules.into_iter().map(normalize_rule_aliases).collect(),
            session_rules: Vec::new(),
            repository_grants: Vec::new(),
            ceilings: Vec::new(),
            inherited: Vec::new(),
            workspace: None,
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
        Self::new(rules)
    }

    /// The plan agent gets the same base rules plus a write-denying ceiling
    /// (plan files excepted). As a ceiling it cannot be re-opened by user
    /// config or session approvals.
    #[must_use]
    pub fn plan_defaults() -> Self {
        let mut engine = Self::build_defaults();
        engine.ceilings = plan_ceiling();
        engine
    }

    pub fn approve_always(&mut self, pattern: impl Into<String>) {
        self.session_rules
            .push(normalize_rule_aliases(PermissionRule {
                pattern: pattern.into(),
                decision: PermissionDecision::Allow,
            }));
    }
    pub fn add_rule(&mut self, rule: PermissionRule) {
        self.rules.push(normalize_rule_aliases(rule));
    }
    pub fn add_repository_grant(&mut self, rule: PermissionRule) {
        self.repository_grants.push(normalize_rule_aliases(rule));
    }
    /// An "always" answer (this session or this workspace). It settles what
    /// the configured rules leave as a question and never reopens their deny.
    pub fn add_grant(&mut self, rule: PermissionRule) {
        self.session_rules.push(normalize_rule_aliases(rule));
    }
    pub fn add_ceiling(&mut self, rule: PermissionRule) {
        self.ceilings.push(normalize_rule_aliases(rule));
    }
    /// Bound this engine by a parent's [`boundary`](Self::boundary): each of
    /// its rulesets refuses what it denies, whatever the others allow.
    pub fn add_inherited_ceiling(
        &mut self,
        boundary: impl IntoIterator<Item = Vec<PermissionRule>>,
    ) {
        self.inherited.extend(
            boundary
                .into_iter()
                .map(|rules| rules.into_iter().map(normalize_rule_aliases).collect()),
        );
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
        self.evaluate_with(operation, false)
    }

    /// [`Self::evaluate_detailed`] for an edit the caller knows widens what an
    /// agent, command or skill may do (its frontmatter's `permission` or
    /// `tools` change): asked like a configuration file edit.
    #[must_use]
    pub fn evaluate_guarded(&self, operation: &str) -> Evaluation {
        self.evaluate_with(operation, true)
    }

    fn evaluate_with(&self, operation: &str, guarded: bool) -> Evaluation {
        let operation = normalize_operation_alias(operation);
        let spellings = self.path_spellings(&operation);
        let names = |rule: &&PermissionRule| rule_names(rule, &operation, &spellings);
        // Ceilings first and independently: their deny is final.
        for ceiling in std::iter::once(&self.ceilings).chain(&self.inherited) {
            if let Some(ceiling) = ceiling.iter().rfind(names)
                && ceiling.decision == PermissionDecision::Deny
            {
                return Evaluation {
                    decision: PermissionDecision::Deny,
                    matched: Some(ceiling.pattern.clone()),
                    protected: false,
                };
            }
        }
        let configured = self.rules.iter().rfind(names);
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
            .filter(|target| guarded || is_config_path(Path::new(target)))
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
            };
        }
        Evaluation {
            decision: matched.map_or(PermissionDecision::Ask, |rule| rule.decision),
            matched: matched.map(|rule| rule.pattern.clone()),
            protected: false,
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
fn rule_names(rule: &PermissionRule, operation: &str, spellings: &[(String, bool)]) -> bool {
    if wildcard_match(&rule.pattern, operation) {
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
/// from bypassing a deny/ask decision.
fn normalize_operation_alias(value: &str) -> std::borrow::Cow<'_, str> {
    for (alias, canonical) in [
        ("todo:", "todowrite:"),
        ("write:", "edit:"),
        ("apply_patch:", "edit:"),
        ("patch:", "edit:"),
        ("multiedit:", "edit:"),
    ] {
        if let Some(suffix) = value.strip_prefix(alias) {
            return std::borrow::Cow::Owned(format!("{canonical}{suffix}"));
        }
    }
    std::borrow::Cow::Borrowed(value)
}

fn normalize_rule_aliases(mut rule: PermissionRule) -> PermissionRule {
    if let Some((permission, target)) = rule.pattern.split_once(':')
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        let home = PathBuf::from(home).to_string_lossy().into_owned();
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

/// Resolve a model-supplied tool path the same way for permission checks and
/// tool execution. Existing paths (including symlinks) are canonicalized;
/// for a new path the nearest existing ancestor is canonicalized and the
/// missing suffix is appended. This is deliberately filesystem-aware so an
/// in-workspace symlink cannot disguise an external target from the
/// `external_directory` gate.
#[must_use]
pub fn resolve_permission_path(workspace: &Path, value: &str) -> PathBuf {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| normalize_absolute(workspace));
    let requested = Path::new(value);
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    };
    let joined = normalize_absolute(&joined);
    if let Ok(path) = joined.canonicalize() {
        return path;
    }

    let mut ancestor = joined.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            return joined;
        };
        suffix.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return joined;
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
    let given = Path::new(value);
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
        rule("cloud_agent_session:*", PermissionDecision::Deny),
        rule("cloud_agent_session:list:*", PermissionDecision::Allow),
        rule("cloud_agent_session:get:*", PermissionDecision::Allow),
        rule(
            "cloud_agent_session:ssh_public_key:*",
            PermissionDecision::Allow,
        ),
        rule("cloud_agent_session:messages:*", PermissionDecision::Allow),
        rule("cloud_agent_session:wait:*", PermissionDecision::Allow),
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
        engine.approve_always("cloud_agent_session:*");
        engine.add_rule(rule("cloud_agent_manage:*", PermissionDecision::Allow));
        for action in ["list", "get", "messages", "wait"] {
            assert_eq!(
                engine.evaluate(&format!("cloud_agent_session:{action}:session-id")),
                PermissionDecision::Allow
            );
        }
        for action in [
            "create", "start", "ask", "stop", "abort", "rename", "archive", "delete",
        ] {
            assert_eq!(
                engine.evaluate(&format!("cloud_agent_session:{action}:session-id")),
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
            engine.approve_always(&format!("{tool}:*"));
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
        build.add_rule(rule("cloud_agent_session:*", PermissionDecision::Deny));
        assert_eq!(
            build.evaluate("cloud_agent_session:ask:session-id"),
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
        // The global configuration folder's own settings and extensions.
        let global = global_config_dir().expect("a home folder");
        for name in [
            "config.toml",
            "extensions.json",
            "extensions/x.js",
            "haarness.json",
        ] {
            assert!(is_config_path(&global.join(name)), "{name}");
        }
        for name in ["AGENTS.md", "commands/x.md", "extensions"] {
            assert!(!is_config_path(&global.join(name)), "{name}");
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
