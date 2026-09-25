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
}

#[derive(Debug, Clone, Default)]
pub struct PermissionEngine {
    rules: Vec<PermissionRule>,
    repository_grants: Vec<PermissionRule>,
    session_rules: Vec<PermissionRule>,
    /// Evaluated independently of the rules: a ceiling `deny` wins even when
    /// a later rule or session approval allows. Used for the plan agent and
    /// for child agents inheriting their parent's boundary.
    ceilings: Vec<PermissionRule>,
}

impl PermissionEngine {
    #[must_use]
    pub fn new(rules: Vec<PermissionRule>) -> Self {
        Self {
            rules: rules.into_iter().map(normalize_rule_aliases).collect(),
            session_rules: Vec::new(),
            repository_grants: Vec::new(),
            ceilings: Vec::new(),
        }
    }

    /// Ordinary tools run by default. External paths and environment secrets
    /// require approval, alongside explicit agent changes and generated media.
    #[must_use]
    pub fn build_defaults() -> Self {
        Self::new(vec![
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
            rule("read:*.env", PermissionDecision::Ask),
            rule("read:*.env.*", PermissionDecision::Ask),
            rule("read:*.env.example", PermissionDecision::Allow),
        ])
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
    pub fn add_repository_grant(&mut self, rule: PermissionRule) { self.repository_grants.push(normalize_rule_aliases(rule)); }
    pub fn add_ceiling(&mut self, rule: PermissionRule) {
        self.ceilings.push(normalize_rule_aliases(rule));
    }
    /// The effective ruleset (rules + session approvals), e.g. to snapshot a
    /// parent's boundary as a child agent's ceiling.
    #[must_use]
    pub fn flattened_rules(&self) -> Vec<PermissionRule> {
        self.repository_grants.iter().chain(&self.rules).chain(&self.session_rules)
            .cloned()
            .collect()
    }

    /// Everything that bounds this engine — effective rules plus its own
    /// ceilings. A child agent inherits this as its ceiling so it can never
    /// escalate past its parent.
    #[must_use]
    pub fn boundary(&self) -> Vec<PermissionRule> {
        let mut rules = self.flattened_rules();
        rules.extend(self.ceilings.iter().cloned());
        rules
    }

    #[must_use]
    pub fn evaluate(&self, operation: &str) -> PermissionDecision {
        self.evaluate_detailed(operation).decision
    }

    #[must_use]
    pub fn evaluate_detailed(&self, operation: &str) -> Evaluation {
        let operation = normalize_operation_alias(operation);
        // Ceilings first and independently: their deny is final.
        if let Some(ceiling) = self
            .ceilings
            .iter()
            .rfind(|rule| wildcard_match(&rule.pattern, operation.as_ref()))
        {
            if ceiling.decision == PermissionDecision::Deny {
                return Evaluation {
                    decision: PermissionDecision::Deny,
                    matched: Some(ceiling.pattern.clone()),
                };
            }
        }
        let matched = self
            .rules
            .iter()
            .chain(&self.session_rules)
            .rfind(|rule| wildcard_match(&rule.pattern, operation.as_ref()));
        let repository = self.repository_grants.iter().rfind(|rule| wildcard_match(&rule.pattern, operation.as_ref()));
        // Sharing consent never reopens an explicit deny in this worktree.
        let matched = if matched.is_none_or(|rule| rule.decision != PermissionDecision::Deny) { repository.or(matched) } else { matched };
        Evaluation {
            decision: matched.map_or(PermissionDecision::Ask, |rule| rule.decision),
            matched: matched.map(|rule| rule.pattern.clone()),
        }
    }

    /// Whether a tool is categorically unavailable and should therefore be
    /// omitted from the model-facing tool schema.
    /// Local/session rules are one ordered ruleset,
    /// inherited ceilings are checked independently, and only a final `*`
    /// deny hides the complete tool. Scoped denies remain visible because a
    /// valid argument can still be allowed at execution time.
    #[must_use]
    pub fn disables_tool(&self, tool: &str) -> bool {
        if let Some(method) = tool.strip_prefix("browser_") {
            return ruleset_disables_browser(self.rules.iter().chain(&self.session_rules), method)
                || ruleset_disables_browser(self.ceilings.iter(), method);
        }
        ruleset_disables_tool(
            self.rules.iter().chain(&self.session_rules),
            canonical_tool_permission(tool),
        ) || ruleset_disables_tool(self.ceilings.iter(), canonical_tool_permission(tool))
    }
}

fn ruleset_disables_browser<'a>(rules: impl DoubleEndedIterator<Item = &'a PermissionRule>, method: &str) -> bool {
    rules.rev().find_map(|rule| {
        let (permission, target) = rule.pattern.split_once(':').unwrap_or((&rule.pattern, "*"));
        if !wildcard_match(permission, "browser") { return None; }
        if target == "*" { return Some(rule.decision == PermissionDecision::Deny); }
        let (operation, resource) = target.split_once(':').unwrap_or((target, "*"));
        wildcard_match(operation, method).then_some(resource == "*" && rule.decision == PermissionDecision::Deny)
    }).unwrap_or(false)
}

fn ruleset_disables_tool<'a>(
    rules: impl DoubleEndedIterator<Item = &'a PermissionRule>,
    tool: &str,
) -> bool {
    rules
        .rev()
        .find_map(|rule| {
            let (permission, target) = rule.pattern.split_once(':').unwrap_or((&rule.pattern, "*"));
            wildcard_match(permission, tool).then_some((target, rule.decision))
        })
        .is_some_and(|(target, decision)| target == "*" && decision == PermissionDecision::Deny)
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
        rule("cloud_agent_manage:ssh_public_key:*", PermissionDecision::Allow),
        rule("cloud_agent_session:*", PermissionDecision::Deny),
        rule("cloud_agent_session:list:*", PermissionDecision::Allow),
        rule("cloud_agent_session:get:*", PermissionDecision::Allow),
        rule("cloud_agent_session:ssh_public_key:*", PermissionDecision::Allow),
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
    fn cloud_plan_reads_cannot_escalate_to_remote_execution() {
        let mut engine = PermissionEngine::plan_defaults();
        engine.approve_always("cloud_agent_session:*");
        engine.add_rule(rule("cloud_agent_manage:*", PermissionDecision::Allow));
        for action in ["list", "get", "messages", "wait"] {
            assert_eq!(engine.evaluate(&format!("cloud_agent_session:{action}:session-id")), PermissionDecision::Allow);
        }
        for action in ["create", "start", "ask", "stop", "abort", "rename", "archive", "delete"] {
            assert_eq!(engine.evaluate(&format!("cloud_agent_session:{action}:session-id")), PermissionDecision::Deny);
        }
        for action in ["list", "get", "ssh_public_key", "models"] {
            assert_eq!(engine.evaluate(&format!("cloud_agent_manage:{action}:agent-id")), PermissionDecision::Allow);
        }
        for action in ["create", "update", "delete"] {
            assert_eq!(engine.evaluate(&format!("cloud_agent_manage:{action}:agent-id")), PermissionDecision::Deny);
        }
        let mut build = PermissionEngine::build_defaults();
        build.add_rule(rule("cloud_agent_session:*", PermissionDecision::Deny));
        assert_eq!(build.evaluate("cloud_agent_session:ask:session-id"), PermissionDecision::Deny);
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
}
