use haarness_protocol::AgentProfile;

/// Stable runtime facts supplied to every model request. Keeping the thread
/// creation date fixed preserves the reusable system-prompt prefix across
/// provider rounds and days; a later turn on another date tells the model the
/// new date in a reminder stored with that turn (see `turn_context`).
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

/// The words before the custom subagents the `task` description lists.
const SUBAGENT_LIST_OPENING: &str = "\n\nCustom subagents you can name in agent:";

/// The words before the custom subagents named without a description.
const SUBAGENT_NAMES_OPENING: &str = "\nMore custom subagents: ";

/// The most bytes the custom subagent list may take (about 1K tokens): it
/// goes with every request of a chat that can start subagents.
pub const SUBAGENT_LIST_BUDGET: usize = 4 * 1024;

/// What the `task` description ends with in a chat (not in a subagent):
/// the custom agents it can start, one line each, so the model learns they
/// exist without the person naming them. Those of mode `subagent` or `all`
/// (the default) that are not hidden, sorted by id; a description is one
/// line, cut to 250 characters. A list over [`SUBAGENT_LIST_BUDGET`]
/// describes the first ones and names the rest, counting those that do not
/// fit. The same agents give the same bytes, so the cached prompt holds.
/// Empty when there is none.
#[must_use]
pub fn subagent_list(agents: &[crate::CustomAgent]) -> String {
    let mut listed = agents
        .iter()
        .filter(|agent| {
            !agent.hidden
                && matches!(agent.mode.as_deref(), None | Some("subagent" | "all"))
                && !agent.id.is_empty()
                && !agent.id.chars().any(char::is_control)
        })
        .collect::<Vec<_>>();
    if listed.is_empty() {
        return String::new();
    }
    listed.sort_by(|left, right| left.id.cmp(&right.id));
    listed.dedup_by(|left, right| left.id == right.id);
    let lines = listed
        .iter()
        .map(|agent| {
            // One line, and no control character a project's file may hold.
            let description = agent
                .description
                .split_whitespace()
                .map(|word| {
                    word.chars()
                        .filter(|character| !character.is_control())
                        .collect::<String>()
                })
                .filter(|word| !word.is_empty())
                .collect::<Vec<_>>();
            if description.is_empty() {
                format!("\n- {}", agent.id)
            } else {
                let description = description.join(" ");
                format!(
                    "\n- {}: {}",
                    agent.id,
                    crate::tool_catalog::shortened(&description)
                )
            }
        })
        .collect::<Vec<_>>();
    let mut text = String::from(SUBAGENT_LIST_OPENING);
    if text.len() + lines.iter().map(String::len).sum::<usize>() <= SUBAGENT_LIST_BUDGET {
        text.push_str(&lines.concat());
        return text;
    }
    // Describe agents while a quarter of the budget is left to name the rest.
    let room = SUBAGENT_LIST_BUDGET / 4;
    let mut count = 0;
    for line in &lines {
        if text.len() + line.len() > SUBAGENT_LIST_BUDGET - room {
            break;
        }
        text.push_str(line);
        count += 1;
    }
    let rest = listed[count..]
        .iter()
        .map(|agent| agent.id.as_str())
        .collect::<Vec<_>>();
    let names_room = SUBAGENT_LIST_BUDGET - text.len() - SUBAGENT_NAMES_OPENING.len() - 1;
    text.push_str(SUBAGENT_NAMES_OPENING);
    text.push_str(&crate::tool_catalog::some_names(&rest, names_room));
    text.push('.');
    text
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
    instructions_with_context(&system_prompt_for(agent), context)
}

/// A built-in agent's instructions (its prompt as prompt files leave it) and
/// the execution environment, which no prompt file changes.
pub(crate) fn instructions_with_context(
    instructions: &str,
    context: &SystemPromptContext<'_>,
) -> String {
    format!("{instructions}\n\n{}", runtime_context_prompt(context))
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
        crate::turn_context::local_date(context.created_at).format("%Y-%m-%d"),
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

    fn agent(id: &str, description: &str, mode: Option<&str>, hidden: bool) -> crate::CustomAgent {
        crate::CustomAgent {
            id: id.into(),
            description: description.into(),
            mode: mode.map(str::to_owned),
            hidden,
            ..crate::CustomAgent::default()
        }
    }

    #[test]
    fn the_subagent_list_names_the_agents_a_chat_can_start() {
        let agents = [
            agent(
                "reviewer",
                "Review diffs after code changes",
                Some("subagent"),
                false,
            ),
            agent("docs", "Write and check docs", None, false),
            agent("helper", "Helps anywhere", Some("all"), false),
            agent("lead", "A primary agent", Some("primary"), false),
            agent("quiet", "Hidden from pickers", Some("subagent"), true),
            agent("odd", "An unknown mode", Some("sometimes"), false),
            agent("bare", "", Some("subagent"), false),
        ];
        assert_eq!(
            subagent_list(&agents),
            "\n\nCustom subagents you can name in agent:\n- bare\n- docs: Write and check docs\n- helper: Helps anywhere\n- reviewer: Review diffs after code changes"
        );
        // The order the agents were defined in does not change the bytes.
        let mut reversed = agents.to_vec();
        reversed.reverse();
        assert_eq!(subagent_list(&reversed), subagent_list(&agents));
        // None to list: nothing is added.
        assert_eq!(subagent_list(&agents[3..6]), "");
        assert_eq!(subagent_list(&[]), "");
    }

    #[test]
    fn a_subagent_description_is_one_line_of_bounded_length() {
        let long = "word ".repeat(200);
        let agents = [
            agent(
                "lines",
                "First line\nsecond line\r\n\n  third\tline \u{1b}[31m\u{7}\u{0} ",
                None,
                false,
            ),
            agent("long", &long, None, false),
            agent("broken\nid", "A name across two lines", None, false),
        ];
        let list = subagent_list(&agents);
        let lines = list.trim_start().lines().collect::<Vec<_>>();
        assert_eq!(
            lines,
            [
                "Custom subagents you can name in agent:",
                "- lines: First line second line third line [31m",
                &format!(
                    "- long: {}",
                    crate::tool_catalog::shortened(long.trim_end())
                ),
            ]
        );
        assert_eq!(lines[2].chars().count(), "- long: ".len() + 250);
    }

    #[test]
    fn a_long_subagent_list_stays_within_its_budget() {
        let description = "Reviews one part of the code base with care. ".repeat(4);
        let agents = (0..300)
            .map(|index| agent(&format!("agent-{index:03}"), &description, None, false))
            .collect::<Vec<_>>();
        let list = subagent_list(&agents);
        assert!(list.len() <= SUBAGENT_LIST_BUDGET, "{}", list.len());
        let (described, rest) = list.split_once(SUBAGENT_NAMES_OPENING).unwrap();
        let shown = described.matches("\n- agent-").count();
        assert!(shown > 5, "{shown}");
        // The next ones by name, the rest counted.
        let (ids, counted) = rest.trim_end_matches('.').rsplit_once(" and ").unwrap();
        let ids = ids.split(", ").collect::<Vec<_>>();
        assert_eq!(ids[0], format!("agent-{shown:03}"));
        assert_eq!(
            counted,
            format!("{} more", agents.len() - shown - ids.len())
        );
        // Many names and no room to describe any: still within the budget.
        let many = (0..2_000)
            .map(|index| agent(&format!("agent-{index:04}"), "", None, false))
            .collect::<Vec<_>>();
        assert!(subagent_list(&many).len() <= SUBAGENT_LIST_BUDGET);
    }

    #[test]
    fn contextual_prompt_is_provider_neutral_and_stable() {
        // Midday UTC is the same date almost everywhere the test may run.
        let created_at = chrono::DateTime::parse_from_rfc3339("2026-08-26T12:02:03Z")
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
