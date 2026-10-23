//! Shared execution settings for every client of a workspace runtime.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionSettings {
    /// Zero disables the deadline. A tool invocation can override this default.
    pub shell_timeout_ms: u64,
    pub media_max_bytes: u64,
    pub subagent_concurrency: usize,
    /// One allows root -> child. Higher values enable nested task agents.
    pub subagent_depth: u8,
    pub read_parallelism: usize,
    /// Zero disables the respective Code Mode budget.
    pub code_timeout_ms: u64,
    pub code_max_calls: usize,
    pub code_max_source_bytes: usize,
    /// Operator-selected extension tools permitted by the plan ceiling.
    pub plan_read_tools: Vec<String>,
    /// Extra environment for one shell tool invocation, supplied per call by
    /// `shell.env` hooks. Never read from or written to configuration.
    #[serde(skip)]
    pub shell_env: std::collections::BTreeMap<String, String>,
    /// The shell command is a person's own (the TUI's `!`), so the shell
    /// tool does not mark it as an agent's (`AI_AGENT`, `AGENT`, `HAARNESS`,
    /// `HAARNESS_SESSION_ID`). Set per call by the runtime, never configured.
    #[serde(skip)]
    pub person_shell: bool,
}

impl Default for ExecutionSettings {
    fn default() -> Self {
        Self {
            shell_timeout_ms: 120_000,
            media_max_bytes: 20 * 1024 * 1024,
            subagent_concurrency: 4,
            subagent_depth: 1,
            read_parallelism: 8,
            code_timeout_ms: 60_000,
            code_max_calls: 100,
            code_max_source_bytes: 256 * 1024,
            plan_read_tools: Vec::new(),
            shell_env: std::collections::BTreeMap::new(),
            person_shell: false,
        }
    }
}

impl ExecutionSettings {
    pub fn validate(&self) -> crate::CoreResult<()> {
        if self.subagent_concurrency == 0 || self.read_parallelism == 0 || self.media_max_bytes == 0
        {
            return Err(crate::CoreError::Invalid(
                "execution concurrency and media_max_bytes must be positive".into(),
            ));
        }
        if self
            .plan_read_tools
            .iter()
            .any(|name| !name.starts_with("ext__") || name.contains(['*', '?', ':']))
        {
            return Err(crate::CoreError::Invalid(
                "execution.plan_read_tools must contain exact ext__ tool names".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_unlimited_budgets_are_explicit_and_validated() {
        let settings: ExecutionSettings = serde_json::from_str(
            r#"{"shell_timeout_ms":0,"code_timeout_ms":0,"code_max_calls":0,"subagent_depth":3}"#,
        )
        .unwrap();
        settings.validate().unwrap();
        assert_eq!(settings.media_max_bytes, 20 * 1024 * 1024);
        for bad in [
            r#"{"read_parallelism":0}"#,
            r#"{"subagent_concurrency":0}"#,
            r#"{"media_max_bytes":0}"#,
            r#"{"plan_read_tools":["ext__*"]}"#,
        ] {
            assert!(
                serde_json::from_str::<ExecutionSettings>(bad)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        assert!(serde_json::from_str::<ExecutionSettings>(r#"{"shell_timout_ms":0}"#).is_err());
    }
    #[test]
    fn hook_supplied_shell_environment_never_reaches_persisted_settings() {
        let mut settings = ExecutionSettings::default();
        settings.shell_env.insert("HOOK_VAR".into(), "1".into());
        settings.person_shell = true;
        let text = serde_json::to_string(&settings).unwrap();
        assert!(!text.contains("shell_env"), "{text}");
        assert!(!text.contains("person_shell"), "{text}");
        assert!(serde_json::from_str::<ExecutionSettings>(r#"{"shell_env":{"A":"b"}}"#).is_err());
        assert!(serde_json::from_str::<ExecutionSettings>(r#"{"person_shell":true}"#).is_err());
    }
    #[test]
    fn plan_observation_and_explicit_extension_allowance_preserve_mutation_ceiling() {
        let mut config = crate::RuntimeConfig::default();
        config
            .execution
            .plan_read_tools
            .push("ext__search__query".into());
        let engine = config.permission_defaults("plan", "");
        for tool in [
            "browser_tabs_list",
            "browser_snapshot",
            "browser_screenshot",
            "browser_find",
            "ext__search__query",
        ] {
            assert!(!engine.disables_tool(tool), "{tool}");
        }
        for tool in [
            "browser_evaluate",
            "browser_click",
            "browser_files_upload",
            "ext__other__write",
        ] {
            assert!(engine.disables_tool(tool), "{tool}");
        }
        assert_eq!(
            engine.evaluate("edit:src/main.rs"),
            crate::PermissionDecision::Deny
        );
    }
}
