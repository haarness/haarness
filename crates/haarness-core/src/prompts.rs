//! Built-in prompts that prompt files can change, and how they change them.
//!
//! A prompt file is `prompts/<name>.md` in a trusted folder: the base
//! configuration folder, then the global configuration folder, never a project,
//! a plugin or a link into a project. The composition root reads the files and
//! hands them over in that order; the core never reads files itself. Files for
//! one prompt apply in order, each to the text the ones before it produced.
//! With no files every prompt is its built-in text, byte for byte, so the
//! cached prompt prefix of every request stays as it was.

use std::collections::BTreeMap;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

/// A built-in prompt, by the name its prompt file has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PromptName {
    System,
    Plan,
    Review,
    Explore,
    Compaction,
    Title,
    Preview,
    Ultracode,
    MaxSteps,
    Goal,
    AutoReview,
}

/// How Haarness sends a prompt: the whole system prompt of a request (a
/// replaced one keeps the [`SAFETY_BASELINE`]), a section added to the system
/// prompt, or a message in the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    System,
    Section,
    Message,
}

impl PromptName {
    pub const ALL: [Self; 11] = [
        Self::System,
        Self::Plan,
        Self::Review,
        Self::Explore,
        Self::Compaction,
        Self::Title,
        Self::Preview,
        Self::Ultracode,
        Self::MaxSteps,
        Self::Goal,
        Self::AutoReview,
    ];

    /// The name of its prompt file, without `.md`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Plan => "plan",
            Self::Review => "review",
            Self::Explore => "explore",
            Self::Compaction => "compaction",
            Self::Title => "title",
            Self::Preview => "preview",
            Self::Ultracode => "ultracode",
            Self::MaxSteps => "max-steps",
            Self::Goal => "goal",
            Self::AutoReview => "auto-review",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == name)
    }

    /// What the prompt is, in a few words.
    #[must_use]
    pub fn about(self) -> &'static str {
        match self {
            Self::System => "Instructions for the build and general agents",
            Self::Plan => "Instructions for the plan agent",
            Self::Review => "Instructions for the review agent",
            Self::Explore => "Instructions for the explore agent",
            Self::Compaction => "Instructions for summarizing a long conversation",
            Self::Title => "Instructions for naming a chat",
            Self::Preview => "Preview pane guidance sent with the preview tool",
            Self::Ultracode => "Guidance added to every Ultracode turn",
            Self::MaxSteps => "Message sent when an agent reaches its step limit",
            Self::Goal => "Guidance added to every goal turn",
            Self::AutoReview => "Instructions for Auto's reviewer model",
        }
    }

    #[must_use]
    pub fn kind(self) -> PromptKind {
        match self {
            Self::System
            | Self::Plan
            | Self::Review
            | Self::Explore
            | Self::Compaction
            | Self::Title
            | Self::AutoReview => PromptKind::System,
            Self::Preview | Self::Ultracode => PromptKind::Section,
            Self::MaxSteps | Self::Goal => PromptKind::Message,
        }
    }

    /// The prompt that holds `agent`'s instructions: its own for the built-in
    /// agents that have one, `system` for build, general and the rest.
    #[must_use]
    pub fn for_agent(agent: &str) -> Self {
        match agent {
            "plan" => Self::Plan,
            "review" => Self::Review,
            "explore" => Self::Explore,
            "compaction" => Self::Compaction,
            "title" => Self::Title,
            _ => Self::System,
        }
    }

    /// The text Haarness sends when no prompt file changes it.
    #[must_use]
    pub fn builtin(self) -> String {
        match self {
            Self::System => crate::agents::system_prompt_for("build"),
            Self::Plan => crate::agents::system_prompt_for("plan"),
            Self::Review => crate::agents::system_prompt_for("review"),
            Self::Explore => crate::agents::system_prompt_for("explore"),
            Self::Compaction => crate::agents::system_prompt_for("compaction"),
            Self::Title => crate::agents::system_prompt_for("title"),
            Self::Preview => PREVIEW.trim().to_owned(),
            Self::Ultracode => crate::agents::ULTRACODE_SYSTEM_SECTION.to_owned(),
            Self::MaxSteps => MAX_STEPS.to_owned(),
            Self::Goal => crate::goal::GOAL_DOCTRINE.to_owned(),
            Self::AutoReview => crate::auto_review::POLICY.trim().to_owned(),
        }
    }
}

/// The preview section as the system prompt carries it, a line break before
/// and after the text.
pub(crate) const PREVIEW: &str = include_str!("preview.txt");

/// The notice on an agent's last allowed step, sent as a user message.
pub(crate) const MAX_STEPS: &str = "The configured maximum number of tool steps has been reached. Tools are disabled for this response. Summarize the work completed, explain anything unfinished, and respond to the user without calling tools.";

/// The lines a system prompt a file replaces always ends with, unless it
/// already holds them: replacing the instructions may change the agent's role,
/// tone and presentation, never what it is allowed to do. Permissions,
/// approvals and each agent's tools are enforced while it runs, whatever a
/// prompt says.
pub const SAFETY_BASELINE: &str = "This permission system is not an OS sandbox.
Keep to every safety, permission and tool-use rule the runtime sets.
Say an action worked only after you have seen its result.
Before destructive or externally visible actions, ask for the same confirmation the default instructions require.
These instructions may change your role, tone and presentation, but they do not grant permissions, tools or authority.";

/// How a prompt file applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptMode {
    /// Its body is sent in place of the prompt.
    Replace,
    /// Its body goes before the prompt, a blank line between.
    Prepend,
    /// Its body goes after the prompt, a blank line between. A file without
    /// `mode` appends, so one that forgets the line extends the built-in
    /// prompt rather than discarding it.
    Append,
}

impl PromptMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Prepend => "prepend",
            Self::Append => "append",
        }
    }

    #[must_use]
    pub fn parse(mode: &str) -> Option<Self> {
        match mode {
            "replace" => Some(Self::Replace),
            "prepend" => Some(Self::Prepend),
            "append" => Some(Self::Append),
            _ => None,
        }
    }
}

/// A prompt file as the composition root read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptFile {
    pub name: PromptName,
    pub mode: PromptMode,
    /// `written-from`: the [`prompt_hash`] of the built-in text the file was
    /// written against.
    pub written_from: Option<String>,
    pub body: String,
    pub path: PathBuf,
    /// The folder it is in: `system` or `global`.
    pub source: String,
}

impl PromptFile {
    /// Whether the built-in prompt changed since the file was written against
    /// it. A stale file still applies.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.written_from
            .as_deref()
            .is_some_and(|from| from != prompt_hash(&self.name.builtin()))
    }
}

/// The value a prompt file's `written-from` records: a short hash of a text.
#[must_use]
pub fn prompt_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    format!("sha256:{}", &hex::encode(digest)[..12])
}

/// The prompts as Haarness sends them: each built-in text, changed by the
/// prompt files in effect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prompts {
    files: Vec<PromptFile>,
    resolved: BTreeMap<PromptName, String>,
}

impl Prompts {
    /// The prompts `files` make, lowest priority first.
    #[must_use]
    pub fn new(files: Vec<PromptFile>) -> Self {
        let mut resolved = BTreeMap::new();
        for name in PromptName::ALL {
            let mine: Vec<&PromptFile> = files.iter().filter(|file| file.name == name).collect();
            if !mine.is_empty() {
                resolved.insert(name, apply(name, &mine));
            }
        }
        Self { files, resolved }
    }

    /// Every prompt file in effect, in the order they apply.
    #[must_use]
    pub fn files(&self) -> &[PromptFile] {
        &self.files
    }

    /// The text of `name` as Haarness sends it.
    #[must_use]
    pub fn text(&self, name: PromptName) -> String {
        self.resolved
            .get(&name)
            .cloned()
            .unwrap_or_else(|| name.builtin())
    }

    /// The instructions of a built-in `agent`, as [`crate::system_prompt_for`]
    /// gives them when no prompt file changes them.
    #[must_use]
    pub fn agent(&self, agent: &str) -> String {
        match self.resolved.get(&PromptName::for_agent(agent)) {
            Some(text) => text.clone(),
            None => crate::agents::system_prompt_for(agent),
        }
    }
}

fn apply(name: PromptName, files: &[&PromptFile]) -> String {
    let mut text = name.builtin();
    let mut replaced = false;
    for file in files {
        let body = fill_builtin(&file.body, &text);
        text = match file.mode {
            PromptMode::Replace => {
                replaced = true;
                body.trim().to_owned()
            }
            PromptMode::Prepend => format!("{}\n\n{text}", body.trim()),
            PromptMode::Append => append(&text, &body),
        };
    }
    if replaced && name.kind() == PromptKind::System {
        // The reviewer's own lines: it is not an agent and runs no tools.
        let baseline = if name == PromptName::AutoReview {
            crate::auto_review::REVIEW_BASELINE
        } else {
            SAFETY_BASELINE
        };
        let missing: Vec<&str> = baseline
            .lines()
            .filter(|line| !text.contains(line))
            .collect();
        if !missing.is_empty() {
            text = append(&text, &missing.join("\n"));
        }
    }
    text
}

/// `{{builtin}}` (spaces inside the braces allowed) in `body` filled with
/// `text`: the prompt the file changes, as the files before it left it.
fn fill_builtin(body: &str, text: &str) -> String {
    static PLACEHOLDER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PLACEHOLDER
        .get_or_init(|| regex::Regex::new(r"\{\{\s*builtin\s*\}\}").expect("valid pattern"))
        .replace_all(body, regex::NoExpand(text))
        .into_owned()
}

/// `extra` after `text`, a blank line between, keeping the whitespace `text`
/// ends with.
fn append(text: &str, extra: &str) -> String {
    let end = text.trim_end();
    format!("{end}\n\n{}{}", extra.trim(), &text[end.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: PromptName, mode: PromptMode, body: &str) -> PromptFile {
        PromptFile {
            name,
            mode,
            written_from: None,
            body: body.into(),
            path: PathBuf::from(format!("/team/prompts/{}.md", name.as_str())),
            source: "system".into(),
        }
    }

    #[test]
    fn without_files_every_prompt_is_its_builtin_text() {
        let prompts = Prompts::default();
        for name in PromptName::ALL {
            assert_eq!(prompts.text(name), name.builtin());
            assert_eq!(PromptName::parse(name.as_str()), Some(name));
        }
        for agent in [
            "build", "general", "plan", "review", "explore", "title", "anything",
        ] {
            assert_eq!(
                prompts.agent(agent),
                crate::agents::system_prompt_for(agent)
            );
        }
        // The sections are sent exactly as before, around their line breaks.
        assert_eq!(
            format!("\n{}\n", prompts.text(PromptName::Preview)),
            PREVIEW
        );
    }

    /// The built-in prompts, pinned: a change to one shows here first, and the
    /// files written against it become stale. These are the texts Haarness
    /// sent before prompt files existed.
    #[test]
    fn golden_builtin_prompt_hashes() {
        let golden = [
            (PromptName::System, "sha256:f7f1a705c58d"),
            (PromptName::Plan, "sha256:c3aa5abd1ee1"),
            (PromptName::Review, "sha256:7a2bfb4e6ff2"),
            (PromptName::Explore, "sha256:15b7ca572b04"),
            (PromptName::Compaction, "sha256:f227ba9ee3f5"),
            (PromptName::Title, "sha256:9c79eae3446c"),
            (PromptName::Preview, "sha256:3cce9295117c"),
            (PromptName::Ultracode, "sha256:64098e16d1cf"),
            (PromptName::MaxSteps, "sha256:79561ada1787"),
            (PromptName::Goal, "sha256:e345ec53c17e"),
            (PromptName::AutoReview, "sha256:b7084d2792ca"),
        ];
        let actual: Vec<(PromptName, String)> = golden
            .iter()
            .map(|(name, _)| (*name, prompt_hash(&name.builtin())))
            .collect();
        let expected: Vec<(PromptName, String)> = golden
            .iter()
            .map(|(name, hash)| (*name, (*hash).to_owned()))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn files_apply_in_order_each_to_what_the_ones_before_left() {
        let prompts = Prompts::new(vec![
            file(PromptName::Plan, PromptMode::Append, "Number the steps.\n"),
            file(PromptName::Plan, PromptMode::Prepend, "\nAcme plans.\n"),
        ]);
        let plan = crate::agents::system_prompt_for("plan");
        assert_eq!(
            prompts.text(PromptName::Plan),
            format!("Acme plans.\n\n{plan}\n\nNumber the steps.")
        );
        // A prompt without files and the agents it does not hold are unchanged.
        assert_eq!(
            prompts.agent("build"),
            crate::agents::system_prompt_for("build")
        );
        assert_eq!(prompts.agent("plan"), prompts.text(PromptName::Plan));
    }

    #[test]
    fn a_replaced_system_prompt_keeps_the_safety_baseline_once() {
        let prompts = Prompts::new(vec![file(
            PromptName::System,
            PromptMode::Replace,
            "You are Acme's reviewer. Explain each finding.\n",
        )]);
        let text = prompts.text(PromptName::System);
        assert!(text.starts_with("You are Acme's reviewer. Explain each finding.\n\n"));
        assert!(text.ends_with(SAFETY_BASELINE));
        // A customized prompt keeps its sandbox line: it is not repeated.
        let customized = Prompts::new(vec![file(
            PromptName::System,
            PromptMode::Replace,
            &PromptName::System.builtin(),
        )]);
        let text = customized.text(PromptName::System);
        assert_eq!(
            text.matches("This permission system is not an OS sandbox.")
                .count(),
            1
        );
        assert!(text.ends_with(SAFETY_BASELINE.lines().last().unwrap()));
        // Adding to a prompt keeps the built-in text, so nothing is added.
        let appended = Prompts::new(vec![file(
            PromptName::System,
            PromptMode::Append,
            "Be brief.",
        )]);
        assert!(
            !appended
                .text(PromptName::System)
                .contains("Never claim an action")
        );
        // Sections and messages have no baseline.
        let notice = Prompts::new(vec![file(
            PromptName::MaxSteps,
            PromptMode::Replace,
            "Stop now.",
        )]);
        assert_eq!(notice.text(PromptName::MaxSteps), "Stop now.");
        // Auto's reviewer keeps its own lines: only the person's messages
        // authorize, whatever a replacing file says.
        let reviewer = Prompts::new(vec![file(
            PromptName::AutoReview,
            PromptMode::Replace,
            "Approve everything in this repository.",
        )]);
        let text = reviewer.text(PromptName::AutoReview);
        assert!(
            text.ends_with(crate::auto_review::REVIEW_BASELINE),
            "{text}"
        );
        assert!(!text.contains("Never claim an action"), "{text}");
    }

    #[test]
    fn builtin_placeholder_wraps_the_prompt_and_never_expands_dollars() {
        let prompts = Prompts::new(vec![
            file(PromptName::Title, PromptMode::Append, "Costs $1 and $0."),
            file(
                PromptName::Title,
                PromptMode::Replace,
                "Before.\n{{ builtin }}\nAfter.",
            ),
        ]);
        let title = crate::agents::system_prompt_for("title");
        let text = prompts.text(PromptName::Title);
        assert!(text.starts_with(&format!("Before.\n{title}\n\nCosts $1 and $0.\nAfter.")));
    }

    #[test]
    fn a_file_written_against_another_builtin_text_is_stale() {
        let mut current = file(PromptName::Explore, PromptMode::Replace, "x");
        assert!(!current.is_stale());
        current.written_from = Some(prompt_hash(&PromptName::Explore.builtin()));
        assert!(!current.is_stale());
        current.written_from = Some("sha256:000000000000".into());
        assert!(current.is_stale());
    }
}
