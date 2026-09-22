use haarness_protocol::AgentProfile;

/// Stable runtime facts supplied to every model request. Keeping the thread
/// creation date fixed preserves the reusable system-prompt prefix across
/// provider rounds while still giving the model the context it needs.
pub struct SystemPromptContext<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub workspace: &'a std::path::Path,
    pub git_repository: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub output_style: Option<&'a crate::OutputStyle>,
}

/// Provider-independent standing guidance for Ultracode
/// sessions. The symbolic effort and orchestration preference stay durable;
/// concrete provider effort is resolved at dispatch time.
pub const ULTRACODE_SYSTEM_SECTION: &str = "Ultracode mode is enabled for this turn. Use the strongest supported reasoning effort, but choose the lightest execution shape that fits the work. Work directly for conversation, a focused edit, or a short sequential task. Use focused task subagents when one or a few isolated side investigations would otherwise flood the main context. Start a dynamic workflow only for work that genuinely needs durable multi-stage coordination, broad independent fan-out, or more workers and cross-checking than a handful of task calls can manage—for example a codebase-wide audit, a large migration, or multi-source research with adversarial verification. Normally start at most one top-level workflow for the request; put understand, change, and verify phases inside it instead of launching successive workflows. Prefer restricted portable inline source for one-off orchestration and persist a reusable workflow only when the user asks. Workflow completion is delivered automatically: do not sleep or poll after launch. Workflow agents must not create nested workflows unless the workflow author explicitly opts them in.";

#[must_use]
pub fn builtin_profiles() -> Vec<AgentProfile> {
    vec![
        profile(
            "build",
            "Build",
            "Implements and verifies changes",
            false,
            4,
        ),
        profile(
            "plan",
            "Plan",
            "Explores and produces a decision-complete plan without mutations",
            false,
            4,
        ),
        profile(
            "review",
            "Review",
            "Reports evidence-backed structured findings",
            false,
            4,
        ),
        delegable(profile(
            "explore",
            "Explore",
            "Performs focused read-only investigation",
            false,
            2,
        )),
        delegable(profile(
            "general",
            "General",
            "General-purpose collaborative assistant",
            false,
            4,
        )),
        profile("title", "Title", "Generates concise thread titles", true, 0),
        profile(
            "compaction",
            "Compaction",
            "Compacts context without deleting source history",
            true,
            0,
        ),
    ]
}

/// A built-in that also answers `@name` mentions and `task` delegation: mode
/// `all` keeps it selectable as the session agent while listing it as a
/// subagent for explicit delegation.
fn delegable(mut profile: AgentProfile) -> AgentProfile {
    profile.mode = Some("all".into());
    profile
}

fn profile(
    id: &str,
    name: &str,
    description: &str,
    hidden: bool,
    max_subagents: u8,
) -> AgentProfile {
    AgentProfile {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        hidden,
        max_subagents,
        mode: None,
        model: None,
    }
}

#[must_use]
pub fn system_prompt_for(agent: &str) -> String {
    let role = match agent {
        "plan" => {
            "Explore first, do not mutate files, and return a decision-complete implementation plan."
        }
        "review" => {
            "Review for correctness and risk. Lead with findings and cite exact files and lines."
        }
        "explore" => "Investigate narrowly using read-only operations and report evidence.",
        "compaction" => {
            "Summarize the conversation so a fresh context can continue seamlessly: goals, \
             decisions, current state, files touched, and outstanding work. Reply with only \
             the summary."
        }
        "title" => {
            "Name this conversation. Reply with only a thread title of at most 50 characters; \
             no quotes, no trailing punctuation."
        }
        _ => {
            "Work autonomously, preserve unrelated user changes, use tools carefully, and verify the result."
        }
    };
    format!(
        "You are Haarness, a local agentic coding harness. {role} This permission system is not an OS sandbox."
    )
}

/// Provider-neutral system scaffold plus the concrete execution environment.
/// Product/runtime facts live here; user and project instructions are layered
/// separately by the runtime and never need vendor-private assumptions.
#[must_use]
pub fn system_prompt_with_context(agent: &str, context: &SystemPromptContext<'_>) -> String {
    format!(
        "{}\n\n{}",
        system_prompt_for(agent),
        runtime_context_prompt(context)
    )
}

/// Custom-agent bodies replace the role guidance but not the product identity,
/// execution environment, or standing tool/safety contract.
pub(crate) fn custom_system_prompt_with_context(
    prompt: &str,
    context: &SystemPromptContext<'_>,
) -> String {
    format!(
        "You are Haarness, a local agentic coding harness. This permission system is not an OS sandbox.\n\n{}\n\n{}",
        prompt.trim(),
        runtime_context_prompt(context),
    )
}

fn runtime_context_prompt(context: &SystemPromptContext<'_>) -> String {
    let mut prompt = format!(
        "You are powered by the model named {}. The exact model ID is {}/{}.\n\
         Here is useful information about the environment you are running in:\n\
         <env>\n\
           Working directory: {}\n\
           Is directory a git repo: {}\n\
           Platform: {}\n\
           Architecture: {}\n\
           Today's date: {}\n\
         </env>\n\n\
         Use native tool calls only; never print invented tool-call markup. Treat the working directory as the default scope, preserve unrelated user changes, and verify material changes in proportion to risk. When tools are unavailable or a check cannot be run, say so plainly instead of claiming success.",
        context.model,
        context.provider,
        context.model,
        context.workspace.display(),
        if context.git_repository { "yes" } else { "no" },
        std::env::consts::OS,
        std::env::consts::ARCH,
        context.created_at.format("%Y-%m-%d"),
    );
    if let Some(style) = context.output_style {
        prompt.push_str("\n\n<output-style name=\"");
        prompt.push_str(&style.id);
        prompt.push_str("\">\n");
        prompt.push_str(style.instructions.trim());
        prompt.push_str("\n</output-style>");
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contextual_prompt_is_provider_neutral_and_stable() {
        let created_at = chrono::DateTime::parse_from_rfc3339("2026-08-26T01:02:03Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let prompt = system_prompt_with_context(
            "build",
            &SystemPromptContext {
                provider: "public-provider",
                model: "model-1",
                workspace: std::path::Path::new("/tmp/project"),
                git_repository: true,
                created_at,
                output_style: None,
            },
        );
        assert!(prompt.contains("public-provider/model-1"));
        assert!(prompt.contains("Working directory: /tmp/project"));
        assert!(prompt.contains("Is directory a git repo: yes"));
        assert!(prompt.contains("Today's date: 2026-08-26"));
        assert!(prompt.contains("Use native tool calls only"));
    }

    #[test]
    fn contextual_prompt_contains_selected_output_style() {
        let style = crate::OutputStyle {
            id: "review-ready".into(),
            instructions: "Lead with findings and include evidence.".into(),
        };
        let prompt = system_prompt_with_context(
            "build",
            &SystemPromptContext {
                provider: "provider",
                model: "model",
                workspace: std::path::Path::new("/tmp/project"),
                git_repository: false,
                created_at: chrono::DateTime::parse_from_rfc3339("2026-09-04T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                output_style: Some(&style),
            },
        );
        assert!(prompt.contains("<output-style name=\"review-ready\">"));
        assert!(prompt.contains("Lead with findings and include evidence."));
    }
}
