//! Transcript rendering: every durable item kind plus the live previews.
//!
//! The visual language: your prompts sit in raised surface blocks marked by
//! the sage bar; the assistant answers in plain prose anchored by a small
//! teal `▸`, with its reasoning in the same column in muted ink; tool
//! activity reads as a status-dot row with a gutter-lined output block.
//! Boxes for activity, open text for the conversation.

use haarness_opentui::{Line, Span, attr};
use haarness_protocol::{FileDiff, FileDiffStatus, Item, ItemData};

use crate::diff::{render_added, render_diff};
use crate::markdown::render_markdown;
use crate::state::{App, ChildTask};
use crate::theme::Theme;
use crate::view::frame::{HitTarget, TaggedLines};
use crate::view::span::{line, single_line, spinner, task_state_style, wrap_spans};

/// Reasoning is the model thinking aloud, not a nested detail of something
/// else, so it sits in the same text column and wraps to the same measure as
/// the prose on either side of it. Colour alone marks it apart.
const REASONING_INDENT: &str = "  ";
/// Collapsed tool output: enough to see what happened, not a dump.
const TOOL_SUMMARY_ROWS: usize = 3;
/// Errors earn more room; the model's failure detail matters.
const TOOL_ERROR_ROWS: usize = 8;
/// A command typed with `!` was run to read its output, so it shows open.
const SHELL_OUTPUT_ROWS: usize = 10;
/// A prompt longer than this, such as a long paste, folds to its opening
/// `USER_PREVIEW_ROWS` rows.
const USER_BLOCK_ROWS: usize = 12;
const USER_PREVIEW_ROWS: usize = 8;
const TOOL_BLOCK_ROWS: usize = 12;
/// A tool name as the server matches it: without a `functions.` namespace,
/// and compared ignoring ASCII case, so a model writing `Read` gets `read`.
fn bare_tool(name: &str) -> &str {
    name.get(..10)
        .filter(|prefix| prefix.eq_ignore_ascii_case("functions."))
        .map_or(name, |_| &name[10..])
}

fn same_tool(name: &str, tool: &str) -> bool {
    bare_tool(name).eq_ignore_ascii_case(tool)
}

/// The argument worth showing beside an ordinary tool name (path, command, …).
/// A search names what it looks for and where, as Desktop's rows do: two
/// greps in `src` must not both read `grep src`.
fn primary_argument(
    language: crate::locale::Language,
    name: &str,
    arguments: &serde_json::Value,
) -> Option<String> {
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
    };
    if (same_tool(name, "grep") || same_tool(name, "glob"))
        && let Some(pattern) = text("pattern")
    {
        return Some(text("path").map_or_else(
            || pattern.to_owned(),
            |path| {
                language.format(
                    "{pattern} in {path}",
                    &[("pattern", pattern), ("path", path)],
                )
            },
        ));
    }
    [
        "path",
        "file_path",
        "filePath",
        "command",
        "pattern",
        "query",
        "url",
        "name",
        "prompt",
    ]
    .iter()
    .find_map(|key| text(key))
    .map(str::to_owned)
}

/// What a tool row names after the tool: [`primary_argument`], or the files
/// of a patch, or a workflow's action and run.
fn tool_target(
    language: crate::locale::Language,
    name: &str,
    arguments: &serde_json::Value,
) -> Option<String> {
    if same_tool(name, "workflow") {
        return Some(workflow_argument(arguments));
    }
    primary_argument(language, name, arguments).or_else(|| {
        same_tool(name, "apply_patch")
            .then(|| patch_argument(arguments))
            .flatten()
    })
}

/// Workflow calls otherwise collapse to the unhelpful word `workflow`. Keep
/// the action and durable identity visible so a child waiting on a background
/// run never looks like an unexplained hang.
fn workflow_argument(arguments: &serde_json::Value) -> String {
    let action = arguments
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("run");
    let target = arguments
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            arguments
                .get("runID")
                .and_then(serde_json::Value::as_str)
                .map(|id| format!("#{}", crate::view::span::short_id(id)))
        })
        .or_else(|| {
            arguments
                .get("source")
                .and_then(serde_json::Value::as_str)
                .and_then(inline_workflow_name)
        });
    target.map_or_else(|| action.to_owned(), |target| format!("{action} {target}"))
}

fn inline_workflow_name(source: &str) -> Option<String> {
    let name = source.find("name")?;
    let value = source[name + "name".len()..]
        .split_once(':')?
        .1
        .trim_start();
    let quote = value
        .chars()
        .next()
        .filter(|quote| matches!(quote, '\'' | '"'))?;
    let rest = &value[quote.len_utf8()..];
    let end = rest.find(quote)?;
    (!rest[..end].is_empty()).then(|| rest[..end].to_owned())
}

/// The children a `batch` call spawned, in launch order: their call ids
/// are the batch call id followed by `:1`, `:2`, …
fn batch_children<'a>(
    app: &'a App,
    name: &str,
    call_id: &str,
) -> Vec<(usize, String, &'a ChildTask)> {
    if name != "batch" {
        return Vec::new();
    }
    let prefix = format!("{call_id}:");
    let mut rows: Vec<(usize, String, &ChildTask)> = app
        .children
        .iter()
        .filter_map(|(key, task)| {
            let index = key.strip_prefix(&prefix)?.parse::<usize>().ok()?;
            Some((index, key.clone(), task))
        })
        .collect();
    rows.sort_by_key(|(index, _, _)| *index);
    rows
}

/// Whether every nested call of a batch is a delegation, in which case the
/// batch row itself has nothing to say that its task rows do not.
fn batch_is_only_tasks(arguments: &serde_json::Value) -> bool {
    arguments
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|calls| {
            !calls.is_empty()
                && calls.iter().all(|call| {
                    call.get("tool")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|tool| same_tool(tool, "task"))
                })
        })
}

/// What the model asked the Nth batch entry to do, from the batch arguments.
fn batch_task_label(arguments: &serde_json::Value, index: usize) -> String {
    let parameters = arguments
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
        .and_then(|calls| calls.get(index.saturating_sub(1)))
        .and_then(|call| call.get("parameters"));
    let description = parameters
        .and_then(|parameters| parameters.get("description"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let agent = parameters
        .and_then(|parameters| parameters.get("agent"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (agent, description) {
        (_, Some(description)) => description.to_owned(),
        (Some(agent), None) => format!("{agent} agent"),
        (None, None) => format!("delegation {index}"),
    }
}

/// Steps a collapsed batch lists before the rest fold into `… +N more`.
const BATCH_STEP_ROWS: usize = 6;

/// One step of a `batch` call as the model asked for it: its position
/// (1-based, as in the child call ids `<batch>:N`), tool, parameters and,
/// once the batch returned, whether it failed.
struct BatchStep<'a> {
    index: usize,
    tool: &'a str,
    parameters: &'a serde_json::Value,
    failed: Option<bool>,
}

/// Each entry of a batch result as (tool, failed). The metadata holds every
/// step's whole output, so it is read in place, and the text output is parsed
/// only for a result without it.
fn batch_outcomes(output: &str, metadata: &serde_json::Value) -> Vec<(String, bool)> {
    let read = |results: &serde_json::Value| {
        results.as_array().map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    let tool = entry.get("tool").and_then(serde_json::Value::as_str);
                    (
                        tool.unwrap_or_default().to_owned(),
                        entry.get("error").is_some(),
                    )
                })
                .collect()
        })
    };
    metadata
        .get("results")
        .and_then(read)
        .or_else(|| {
            serde_json::from_str::<serde_json::Value>(output)
                .ok()
                .and_then(|payload| payload.get("results").and_then(read))
        })
        .unwrap_or_default()
}

/// The steps of a `batch` call, matched to its result by position. The
/// server returns results in input order; an entry counts only when it names
/// the step's tool, as Desktop's `batchSteps` matches them. A result's
/// entries are read once and kept, since the transcript is drawn every frame.
fn batch_steps<'a>(
    app: &App,
    arguments: &'a serde_json::Value,
    result: Option<&Item>,
) -> Vec<BatchStep<'a>> {
    let Some(calls) = arguments
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let outcomes = result.and_then(|item| match &item.data {
        ItemData::ToolResult {
            output, metadata, ..
        } => Some(app.cached_batch_results(item.id, || batch_outcomes(output, metadata))),
        _ => None,
    });
    calls
        .iter()
        .enumerate()
        .filter_map(|(index, call)| {
            let tool = bare_tool(call.get("tool")?.as_str()?);
            let failed = outcomes
                .as_ref()
                .and_then(|outcomes| outcomes.get(index))
                .filter(|(name, _)| same_tool(name, tool))
                .map(|(_, failed)| *failed);
            Some(BatchStep {
                index: index + 1,
                tool,
                parameters: call.get("parameters").unwrap_or(&serde_json::Value::Null),
                failed,
            })
        })
        .collect()
}

/// How many steps of a finished batch failed. The batch call itself
/// succeeds and reports its failed steps in `failures`.
fn batch_failures(name: &str, result: Option<&Item>, steps: &[BatchStep]) -> usize {
    if name != "batch" {
        return 0;
    }
    let reported = result.and_then(|item| match &item.data {
        ItemData::ToolResult {
            is_error: false,
            metadata,
            ..
        } => metadata.get("failures").and_then(serde_json::Value::as_u64),
        _ => None,
    });
    reported.map_or_else(
        || {
            steps
                .iter()
                .filter(|step| step.failed == Some(true))
                .count()
        },
        |count| usize::try_from(count).unwrap_or(usize::MAX),
    )
}

/// The line under a delegation row: `↳` what the child is doing while it
/// runs, `└` what it did once it stopped. The status line already says how
/// to open child views, so the row does not repeat it.
fn delegation_detail_line(task: &ChildTask, theme: &Theme) -> Line {
    let tools = match task.tool_calls {
        0 => "no tools yet".to_owned(),
        1 => "1 tool".to_owned(),
        count => format!("{count} tools"),
    };
    let (text, tone) = if task.is_active() {
        let doing = task
            .last_tool
            .as_deref()
            .map_or_else(|| "starting".to_owned(), str::to_owned);
        let elapsed = task
            .elapsed_seconds()
            .map_or_else(String::new, |seconds| format!(" · {seconds}s"));
        match task.state.as_str() {
            "waiting_permission" => (
                format!("↳ waiting for approval · {tools}{elapsed}"),
                theme.accent,
            ),
            "background" => (
                format!("↳ running in background · {doing} · {tools}{elapsed}"),
                theme.muted,
            ),
            _ => (format!("↳ {doing} · {tools}{elapsed}"), theme.muted),
        }
    } else {
        match task.state.as_str() {
            "done" => (format!("└ {tools}"), theme.muted),
            "failed" => (format!("└ failed · {tools}"), theme.danger),
            _ => (format!("└ cancelled · {tools}"), theme.muted),
        }
    };
    Line::new(vec![Span::new("    ", tone), Span::new(text, tone)])
}

fn workflow_run_id(
    arguments: &serde_json::Value,
    result: Option<&Item>,
) -> Option<haarness_protocol::WorkflowId> {
    arguments
        .get("runID")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| id.parse::<uuid::Uuid>().ok())
        .map(haarness_protocol::WorkflowId)
        .or_else(|| {
            let output = result.and_then(|item| match &item.data {
                ItemData::ToolResult { output, .. } => Some(output.as_str()),
                _ => None,
            })?;
            output
                .split(|character: char| !(character.is_ascii_alphanumeric() || character == '-'))
                .find_map(|word| word.parse::<uuid::Uuid>().ok())
                .map(haarness_protocol::WorkflowId)
        })
}

/// Nearest preceding tool call paired with this result. Provider call IDs are
/// not globally unique: some adapters reuse them in later turns, and goal
/// continuations can even reuse them inside one durable turn.
fn paired_call<'a>(app: &'a App, result: &Item, call_id: &str) -> Option<&'a Item> {
    app.items.iter().rev().find(|item| {
        item.sequence < result.sequence
            && item.turn_id == result.turn_id
            && matches!(
                &item.data,
                ItemData::ToolCall {
                    call_id: candidate,
                    ..
                } if candidate == call_id
            )
    })
}

fn paired_result<'a>(app: &'a App, call: &Item, call_id: &str) -> Option<&'a Item> {
    app.items.iter().find(|item| {
        item.sequence > call.sequence
            && item.turn_id == call.turn_id
            && matches!(
                &item.data,
                ItemData::ToolResult {
                    call_id: result_id,
                    ..
                } if result_id == call_id
            )
    })
}

/// Empty machine-shaped values are not useful transcript prose. Keep the tool
/// activity row, but replace raw JSON punctuation with one quiet semantic
/// label. Blank command output is a different state from an empty collection.
fn empty_result_label(output: &str) -> Option<&'static str> {
    let output = output.trim();
    if output.is_empty() {
        return Some("no output");
    }
    match serde_json::from_str::<serde_json::Value>(output) {
        Ok(serde_json::Value::Null) => Some("no results"),
        Ok(serde_json::Value::Array(values)) if values.is_empty() => Some("no results"),
        Ok(serde_json::Value::Object(values)) if values.is_empty() => Some("no results"),
        Ok(serde_json::Value::String(value)) if value.is_empty() => Some("no results"),
        _ => None,
    }
}

fn file_diff_count(metadata: &serde_json::Value) -> usize {
    file_diffs(metadata).len()
}

fn file_diffs(metadata: &serde_json::Value) -> Vec<FileDiff> {
    metadata
        .get("diffs")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<FileDiff>>(value).ok())
        .unwrap_or_default()
}

/// Files touched, and lines added and removed across them, counted from the
/// changed lines rather than the region an edit replaced.
fn file_diff_stat(metadata: &serde_json::Value) -> (usize, u64, u64) {
    file_diffs(metadata)
        .iter()
        .fold((0, 0, 0), |(files, added, removed), diff| {
            let (additions, deletions) = crate::diffview::line_stat(diff);
            (files + 1, added + additions, removed + deletions)
        })
}

/// The files an `apply_patch` envelope names, so its row says what it
/// changes instead of a bare tool name.
fn patch_argument(arguments: &serde_json::Value) -> Option<String> {
    let patch = ["input", "patch"]
        .iter()
        .find_map(|key| arguments.get(key).and_then(serde_json::Value::as_str))?;
    let files: Vec<&str> = patch
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        })
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .collect();
    match files.as_slice() {
        [] => None,
        [only] => Some((*only).to_owned()),
        [first, rest @ ..] => Some(format!("{first} and {} more", rest.len())),
    }
}

/// A tool result that only says the user turned the request down. The
/// server words it for the model ("…take a different approach…").
fn rejected_by_user(output: &str) -> bool {
    output
        .trim_start()
        .starts_with("The user rejected this action")
}

/// A command the person typed in shell mode (`!`), as opposed to one the
/// agent chose to run.
fn is_user_shell(call_id: &str) -> bool {
    call_id.starts_with("shell-")
}

/// The exit code of a command that ran and failed. The bash tool reports a
/// non-zero exit as a successful call, since the command did run, but the
/// row has to read as failed all the same.
fn failed_exit(name: &str, metadata: &serde_json::Value) -> Option<i64> {
    if name != "bash" {
        return None;
    }
    metadata
        .get("exit")
        .and_then(serde_json::Value::as_i64)
        .filter(|code| *code != 0)
}

/// Tool output as a reader wants it. The bash tool opens with an `exit: N`
/// line for the model; the row's status already says how the command ended.
fn displayed_output<'a>(name: &str, output: &'a str, metadata: &serde_json::Value) -> &'a str {
    if name != "bash" || metadata.get("exit").is_none() {
        return output;
    }
    let Some(rest) = output.strip_prefix("exit: ") else {
        return output;
    };
    let (code, body) = rest.split_once('\n').unwrap_or((rest, ""));
    if code.trim().parse::<i64>().is_ok() {
        body
    } else {
        output
    }
}

/// A successful result that carries an MCP App page (`metadata.mcp_app`, a
/// `ui://` page on the tool's server), which only Desktop renders.
fn has_mcp_app(result: &ItemData) -> bool {
    let ItemData::ToolResult {
        is_error, metadata, ..
    } = result
    else {
        return false;
    };
    !is_error
        && metadata
            .pointer("/mcp_app/uri")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|uri| uri.starts_with("ui://"))
}

/// Every successful paired tool result is a compact activity row by default,
/// regardless of tool name or output length. This prevents arbitrary provider
/// formatting from changing the transcript hierarchy. Errors (including a
/// command that exited non-zero) stay visible, and so does the output of a
/// command the person ran themselves; `/tools show` and per-row expansion
/// reveal successful details.
fn folds_into_call_row(app: &App, result: &ItemData) -> bool {
    let ItemData::ToolResult {
        call_id,
        name,
        output,
        is_error,
        metadata,
    } = result
    else {
        return false;
    };
    !is_error
        && failed_exit(name, metadata).is_none()
        && !is_user_shell(call_id)
        && !app.tool_details
        && (empty_result_label(displayed_output(name, output, metadata)).is_none()
            || file_diff_count(metadata) > 0)
}

/// Render only host-produced file bodies. Tool arguments describe intent and
/// can differ from what hit disk (notably multi-file patches and overwrites).
/// Returning an empty vector is deliberate: the caller then renders the
/// textual tool result, preserving older history and third-party tools that
/// do not publish structured diffs.
fn render_file_diffs(metadata: &serde_json::Value, width: usize, theme: &Theme) -> Vec<Line> {
    let Some(raw) = metadata.get("diffs").cloned() else {
        return Vec::new();
    };
    let Ok(diffs) = serde_json::from_value::<Vec<FileDiff>>(raw) else {
        return Vec::new();
    };
    let mut rendered = Vec::new();
    for diff in diffs {
        let mut body = if diff.binary {
            vec![line("      binary content changed", theme.muted)]
        } else {
            match (diff.status, diff.before.as_deref(), diff.after.as_deref()) {
                (FileDiffStatus::Added, _, Some(after)) => {
                    let lines = render_added(after, width, theme, "    ", TOOL_BLOCK_ROWS);
                    if lines.is_empty() {
                        vec![line("      empty file", theme.muted)]
                    } else {
                        lines
                    }
                }
                (FileDiffStatus::Deleted, Some(before), _) => {
                    let lines = render_diff(before, "", width, theme, "    ");
                    if lines.is_empty() {
                        vec![line("      empty file", theme.muted)]
                    } else {
                        lines
                    }
                }
                (FileDiffStatus::Modified, Some(before), Some(after)) => {
                    render_diff(before, after, width, theme, "    ")
                }
                _ => Vec::new(),
            }
        };
        let metadata_only = body.is_empty() && diff.truncated;
        if metadata_only {
            body.push(line("      diff content unavailable", theme.muted));
        }
        // A metadata entry without a usable body must not hide the ordinary
        // textual result. This also treats no-op overwrites as ordinary tool
        // acknowledgements instead of drawing a misleading empty diff block.
        if body.is_empty() {
            continue;
        }
        let (marker, color) = match diff.status {
            FileDiffStatus::Added => ('A', theme.diff.added),
            FileDiffStatus::Deleted => ('D', theme.diff.removed),
            FileDiffStatus::Modified => ('M', theme.accent),
        };
        rendered.push(line(
            format!(
                "    {marker} {}",
                single_line(&diff.file, width.saturating_sub(7))
            ),
            color,
        ));
        rendered.extend(body);
        if diff.truncated && !diff.binary && !metadata_only {
            rendered.push(line("      diff content truncated", theme.muted));
        }
    }
    rendered
}

/// What the person reads when a provider refused images and later requests
/// leave them out.
fn media_omitted_notice(
    language: crate::locale::Language,
    names: &[String],
    guessed: bool,
) -> String {
    let list = names.join(", ");
    if !guessed {
        language.format(
            "The provider refused {name}, so it is left out of this chat from now on.",
            &[("name", &list)],
        )
    } else if names.len() == 1 {
        language.format(
            "The provider refused an image without saying which, so the newest one, {name}, is left out of this chat from now on.",
            &[("name", &list)],
        )
    } else {
        language.format(
            "The provider refused an image without saying which, so the newest ones, {names}, are left out of this chat from now on.",
            &[("names", &list)],
        )
    }
}

#[cfg(test)]
mod workflow_tests {
    use haarness_protocol::{FileDiff, FileDiffStatus};

    use crate::theme::Theme;

    fn file_diff(
        file: &str,
        status: FileDiffStatus,
        before: Option<&str>,
        after: Option<&str>,
    ) -> FileDiff {
        FileDiff {
            file: file.into(),
            status,
            additions: 1,
            deletions: 1,
            patch: None,
            before: before.map(str::to_owned),
            after: after.map(str::to_owned),
            binary: false,
            truncated: false,
        }
    }

    #[test]
    fn workflow_rows_explain_start_and_wait_calls() {
        assert_eq!(
            super::workflow_argument(&serde_json::json!({
                "action": "start",
                "source": "export const meta = { name: 'security-audit' };"
            })),
            "start security-audit"
        );
        assert_eq!(
            super::workflow_argument(&serde_json::json!({
                "action": "wait",
                "runID": "01a036bc-b1d5-7300-a94c-85df55c690f8"
            })),
            "wait #55c690f8"
        );
    }

    #[test]
    fn mutation_results_render_actual_multi_file_and_overwrite_diffs() {
        let metadata = serde_json::json!({
            "diffs": [
                file_diff(
                    "notes.txt",
                    FileDiffStatus::Modified,
                    Some("before\n"),
                    Some("after\n"),
                ),
                file_diff(
                    "new.txt",
                    FileDiffStatus::Added,
                    None,
                    Some("created\n"),
                ),
                file_diff(
                    "old.txt",
                    FileDiffStatus::Deleted,
                    Some("obsolete\n"),
                    None,
                ),
            ]
        });
        let rows: Vec<String> = super::render_file_diffs(&metadata, 80, &Theme::default())
            .iter()
            .map(haarness_opentui::Line::text)
            .collect();
        assert!(
            rows.iter().any(|row| row.contains("M notes.txt")),
            "{rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains("- before")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("+ after")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("A new.txt")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("+ created")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("D old.txt")), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("- obsolete")),
            "{rows:?}"
        );
    }

    #[test]
    fn missing_or_malformed_diff_metadata_yields_to_textual_result() {
        let theme = Theme::default();
        assert!(super::render_file_diffs(&serde_json::Value::Null, 80, &theme).is_empty());
        assert!(
            super::render_file_diffs(&serde_json::json!({"diffs": "not-an-array"}), 80, &theme)
                .is_empty()
        );
        assert!(
            super::render_file_diffs(
                &serde_json::json!({
                    "diffs": [file_diff(
                        "unknown.txt",
                        FileDiffStatus::Modified,
                        None,
                        None,
                    )]
                }),
                80,
                &theme,
            )
            .is_empty()
        );
    }
}

struct QuestionView<'a> {
    id: &'a str,
    prompt: &'a str,
    choices: &'a [String],
    multiple: bool,
    custom: bool,
}

fn render_question(
    app: &App,
    transcript: &mut TaggedLines,
    question: QuestionView<'_>,
    width: usize,
) {
    let theme = &app.theme;
    let active = app
        .first_pending_question()
        .is_some_and(|pending| pending.question_id == question.id);
    transcript.push(line(
        format!(
            "  ? {}",
            single_line(question.prompt, width.saturating_sub(6))
        ),
        theme.accent,
    ));
    if active && app.pending_questions.len() > 1 {
        transcript.push(line(
            format!("    1 of {} questions pending", app.pending_questions.len()),
            theme.muted,
        ));
    }
    if active && question.multiple {
        transcript.push(line(
            if question.custom {
                "    ↑↓ move · space toggle · enter confirm · type custom"
            } else {
                "    ↑↓ move · space toggle · enter confirm"
            },
            theme.muted,
        ));
    }
    for (index, choice) in question.choices.iter().enumerate() {
        let focused = active
            && (question.multiple || !question.custom || app.composer.is_empty())
            && index == app.question_selection.min(question.choices.len() - 1);
        let checked = active && question.multiple && app.question_selected.contains(&index);
        let mut row = line(
            if question.multiple {
                format!(
                    "  {} {} {}. {}",
                    if focused { '›' } else { ' ' },
                    if checked { "[✓]" } else { "[ ]" },
                    index + 1,
                    single_line(choice, width.saturating_sub(14))
                )
            } else {
                format!(
                    "  {} {}. {}",
                    if focused { '›' } else { ' ' },
                    index + 1,
                    single_line(choice, width.saturating_sub(10))
                )
            },
            theme.text,
        );
        if focused {
            crate::view::span::selected_row(&mut row, width, theme);
        }
        if active {
            transcript.push_tagged(row, HitTarget::QuestionChoice(index));
        } else {
            transcript.push(row);
        }
    }
}

fn render_tool_result(
    app: &App,
    transcript: &mut TaggedLines,
    item: &Item,
    call_name: &str,
    width: usize,
) {
    let ItemData::ToolResult {
        call_id,
        name,
        output,
        is_error,
        metadata,
    } = &item.data
    else {
        return;
    };
    let theme = &app.theme;
    let failed = *is_error || failed_exit(name, metadata).is_some();
    let folds = folds_into_call_row(app, &item.data);
    let output = displayed_output(name, output, metadata);

    let expanded = app.expanded.contains(&item.id);

    // Successful mutations expose the host-observed diff when the user asks
    // for details, rather than forcing every diff open in the chat by default.
    if !is_error
        && (expanded || app.tool_details)
        && matches!(call_name, "edit" | "apply_patch" | "write")
    {
        let rendered =
            app.cached_render(item.id, width, || render_file_diffs(metadata, width, theme));
        if !rendered.is_empty() {
            transcript.extend(rendered);
            return;
        }
    }

    // Folded into the call row: a quiet success, a request the user
    // rejected, whose result is the instruction written for the model, or a
    // call the user's stop cancelled, which the row already says.
    if !expanded
        && (folds
            || (*is_error && (rejected_by_user(output) || output.trim() == "operation cancelled")))
        && paired_call(app, item, call_id).is_some()
    {
        return;
    }
    if empty_result_label(output).is_some() {
        return;
    }

    // Results live directly beneath their call, never in a second detached
    // batch. Errors earn more room; all blocks can expand in place.
    let color = if failed { theme.danger } else { theme.muted };
    let collapsed_cap = if is_user_shell(call_id) {
        SHELL_OUTPUT_ROWS
    } else if failed {
        TOOL_ERROR_ROWS
    } else {
        TOOL_SUMMARY_ROWS
    };
    let cap = if expanded { usize::MAX } else { collapsed_cap };
    let rows: Vec<&str> = output.lines().collect();
    let collapsible = folds || rows.len() > collapsed_cap;
    for row in rows.iter().take(cap) {
        let row = gutter_row(row, width, color, theme);
        if collapsible {
            transcript.push_tagged(row, HitTarget::ToolToggle(item.id));
        } else {
            transcript.push(row);
        }
    }
    if rows.len() > cap {
        transcript.push_tagged(
            gutter_row(
                &format!("… +{} more lines · click to expand", rows.len() - cap),
                width,
                theme.muted,
                theme,
            ),
            HitTarget::ToolToggle(item.id),
        );
    } else if expanded && rows.len() > TOOL_SUMMARY_ROWS {
        transcript.push_tagged(
            gutter_row("… click to collapse", width, theme.muted, theme),
            HitTarget::ToolToggle(item.id),
        );
    }
}

/// Anchor the first line of an assistant reply with a small identity mark,
/// so even one-word answers read as "haarness said this".
fn mark_reply(lines: &mut [Line], theme: &Theme) {
    // Keep the anchor on visible content even if another block renderer adds
    // a leading boundary row in the future.
    let Some(first) = lines.iter_mut().find(|line| !blank(line)) else {
        return;
    };
    if let Some(span) = first.spans.first_mut() {
        if let Some(rest) = span.text.strip_prefix("  ") {
            span.text = rest.to_owned();
        }
    }
    first.spans.insert(0, Span::new("▸ ", theme.primary));
}

/// The user's prompt as a raised block: sage bar, surface background, padded
/// to the content width. It hangs off the same margin as the assistant's
/// marker, so both sides of the conversation share one text column and tool
/// activity is the only thing that indents.
///
/// Files and agents attached with `@` show as the chips they were typed as;
/// the snapshot or delegation they expand to is for the model.
fn user_block(text: &str, width: usize, theme: &Theme) -> Vec<Line> {
    use crate::width::DisplayWidth as _;
    let inner = width.saturating_sub(4).max(10);
    let mut paragraphs: Vec<Vec<Span>> = vec![Vec::new()];
    for part in crate::prompt_context::parts(text) {
        match part {
            crate::prompt_context::Part::Text(typed) => {
                for (index, piece) in typed.split('\n').enumerate() {
                    if index > 0 {
                        paragraphs.push(Vec::new());
                    }
                    let piece = piece.trim_end_matches('\r');
                    if !piece.is_empty() {
                        paragraphs
                            .last_mut()
                            .expect("one paragraph")
                            .push(Span::new(piece, theme.text));
                    }
                }
            }
            crate::prompt_context::Part::Reference { label, .. } => paragraphs
                .last_mut()
                .expect("one paragraph")
                .push(Span::new(label, theme.primary).with_attributes(attr::BOLD)),
        }
    }
    if paragraphs.last().is_some_and(Vec::is_empty) && text.ends_with('\n') {
        paragraphs.pop();
    }
    let mut lines = Vec::new();
    for spans in paragraphs {
        let rows = if spans.iter().all(|span| span.text.trim().is_empty()) {
            vec![Line::new(Vec::new())]
        } else {
            wrap_spans(&spans, inner, "", "", theme.text)
        };
        for row in rows {
            let used = row
                .spans
                .iter()
                .map(|span| span.text.width())
                .sum::<usize>();
            let mut spans = vec![Span::new("▎ ", theme.user).on(theme.surface)];
            spans.extend(
                row.spans
                    .into_iter()
                    .filter(|span| !span.text.is_empty())
                    .map(|span| span.on(theme.surface)),
            );
            spans.push(
                Span::new(" ".repeat(inner.saturating_sub(used) + 1), theme.text).on(theme.surface),
            );
            lines.push(Line::new(spans));
        }
    }
    if lines.is_empty() {
        lines.push(Line::empty());
    }
    lines
}

/// Muted gutter row inside a tool output block.
fn gutter_row(text: &str, width: usize, color: haarness_opentui::Color, theme: &Theme) -> Line {
    Line::new(vec![
        // A hairline quote bar, not a box edge: tool output is subordinate
        // content hanging off the activity row, the same way a prompt hangs
        // off its own bar.
        Span::new("  ▏ ", theme.hairline()),
        Span::new(single_line(text, width.saturating_sub(6)), color),
    ])
}

/// Older runtimes persisted every tool call as it streamed but buffered the
/// same-round assistant prose until the response ended. Put that introduction
/// before its activity for every tool kind, matching Haarness's streamed-part
/// ordering. Never carry calls across their first result or later activity:
/// doing so would move completed work below the parent's final answer.
fn presentation_order(items: &[Item]) -> Vec<&Item> {
    let mut ordered = Vec::with_capacity(items.len());
    let mut calls = Vec::new();
    for item in items {
        if matches!(
            item.data,
            ItemData::UserMessage {
                synthetic: true,
                ..
            }
        ) {
            continue;
        }
        match &item.data {
            ItemData::ToolCall { .. } => calls.push(item),
            ItemData::AssistantMessage { .. } if !calls.is_empty() => {
                ordered.push(item);
                ordered.append(&mut calls);
            }
            // These are durable lifecycle/accounting records, not subsequent
            // visible work. They may sit between a call and same-round prose.
            ItemData::TaskLink { .. }
            | ItemData::Usage { .. }
            | ItemData::Snapshot { .. }
            | ItemData::QueueConsumed { .. }
                if !calls.is_empty() =>
            {
                ordered.push(item);
            }
            _ if !calls.is_empty() => {
                ordered.append(&mut calls);
                ordered.push(item);
            }
            _ => ordered.push(item),
        }
    }
    ordered.append(&mut calls);
    ordered
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TranscriptGroup {
    User,
    Assistant,
    Reasoning,
    Tool,
    /// A command the person ran with `!`: its own block, like a prompt.
    Shell,
    Task,
    Plan,
    Review,
    Question,
    Permission,
    System,
}

fn transcript_group(item: &Item) -> TranscriptGroup {
    match &item.data {
        ItemData::UserMessage { .. } => TranscriptGroup::User,
        ItemData::AssistantMessage { .. } => TranscriptGroup::Assistant,
        ItemData::Reasoning { .. } => TranscriptGroup::Reasoning,
        ItemData::ToolCall { name, .. } if name == "task" => TranscriptGroup::Task,
        ItemData::ToolCall { call_id, .. } if is_user_shell(call_id) => TranscriptGroup::Shell,
        ItemData::ToolCall { .. } | ItemData::ToolResult { .. } => TranscriptGroup::Tool,
        ItemData::Plan { .. } => TranscriptGroup::Plan,
        ItemData::Review { .. } => TranscriptGroup::Review,
        ItemData::Question { .. } => TranscriptGroup::Question,
        ItemData::PermissionRequest { .. } => TranscriptGroup::Permission,
        _ => TranscriptGroup::System,
    }
}

fn blank(line: &Line) -> bool {
    line.text().trim().is_empty()
}

/// Append one semantic transcript block with exactly one boundary row. Tool
/// calls (and task calls) remain dense within their own activity group; every
/// other rendered item is a distinct reading block. Outer whitespace emitted
/// by Markdown or specialized renderers is normalized without touching blank
/// lines inside the block.
fn append_block(
    transcript: &mut TaggedLines,
    mut block: TaggedLines,
    group: TranscriptGroup,
    previous: &mut Option<TranscriptGroup>,
) {
    let leading = block.lines.iter().take_while(|line| blank(line)).count();
    if leading > 0 {
        block.lines.drain(..leading);
        block.tags.retain_mut(|(row, _)| {
            if *row < leading {
                false
            } else {
                *row -= leading;
                true
            }
        });
        block.ranged.retain_mut(|entry| {
            if entry.0 < leading {
                false
            } else {
                entry.0 -= leading;
                true
            }
        });
    }
    while block.lines.last().is_some_and(blank) {
        block.lines.pop();
    }
    block.tags.retain(|(row, _)| *row < block.lines.len());
    block.ranged.retain(|entry| entry.0 < block.lines.len());
    if block.is_empty() {
        return;
    }

    let dense = matches!(
        (*previous, group),
        (Some(TranscriptGroup::Tool), TranscriptGroup::Tool)
            | (Some(TranscriptGroup::Task), TranscriptGroup::Task)
    );
    while transcript.lines.last().is_some_and(blank) {
        transcript.lines.pop();
    }
    if !transcript.is_empty() && !dense {
        transcript.push(Line::empty());
    }
    transcript.append(block);
    *previous = Some(group);
}

/// A todo snapshot, or the tool activity that reads or writes one.
fn is_plan_update(item: &Item) -> bool {
    matches!(&item.data, ItemData::Todo { .. })
        || matches!(&item.data,
            ItemData::ToolCall { name, .. } | ItemData::ToolResult { name, .. }
            if matches!(name.as_str(), "todo" | "todowrite" | "todoread"))
}

/// Whether the plan still has work in it; the sidebar shows only such a plan.
fn open_plan(app: &App) -> bool {
    app.todos
        .iter()
        .any(|entry| matches!(entry.status.as_str(), "pending" | "in_progress"))
}

/// The plan in one row: the step in hand, how far along it is, and the key
/// that shows all of it.
fn plan_row(app: &App, width: usize) -> Line {
    use crate::width::DisplayWidth as _;
    let theme = &app.theme;
    let current = app
        .todos
        .iter()
        .find(|entry| entry.status == "in_progress")
        .or_else(|| app.todos.iter().find(|entry| entry.status == "pending"));
    let (marker, tone) = match current.map(|entry| entry.status.as_str()) {
        Some("in_progress") => ("  [•] ", theme.accent),
        _ => ("  [ ] ", theme.muted),
    };
    let done = app
        .todos
        .iter()
        .filter(|entry| entry.status == "completed")
        .count();
    let progress = format!(
        "  ·  {}",
        app.language.format(
            "{done}/{total} done",
            &[
                ("done", &done.to_string()),
                ("total", &app.todos.len().to_string()),
            ]
        )
    );
    let hint = format!("  ·  {}", app.language.text("ctrl+b shows the plan"));
    let room = |suffix: usize| width.saturating_sub(marker.width() + suffix);
    // The step itself matters more than the hint on a narrow terminal.
    let hint = if room(progress.width() + hint.width()) >= 16 {
        hint
    } else {
        String::new()
    };
    let content = current.map_or("", |entry| entry.content.as_str());
    Line::new(vec![
        Span::new(marker, tone),
        Span::new(
            single_line(content, room(progress.width() + hint.width()).max(8)),
            theme.text,
        ),
        Span::new(progress, theme.muted),
        Span::new(hint, theme.muted),
    ])
}

pub fn transcript_lines(app: &App, width: usize) -> TaggedLines {
    let theme = &app.theme;
    let mut transcript = TaggedLines::default();
    let mut previous_group = None;
    // Follow-ups the running turn has not consumed yet wait in the strip
    // above the composer instead; they take their durable place here the
    // moment a QueueConsumed marker lands.
    let waiting: Vec<_> = app
        .pending_follow_ups()
        .iter()
        .map(|item| item.id)
        .collect();
    // Undone work stays on the server until the next turn confirms it, but
    // it is no longer the conversation: fold it into one marker, as Upstream
    // does, instead of drawing it like live turns.
    let undone_after = app.thread.revert.as_ref().map(|revert| revert.sequence);
    let mut folded = false;
    let ordered = presentation_order(&app.items);
    // Off screen, the sidebar leaves one row in its place: the plan's
    // progress where it last changed, which opens the sidebar.
    let plan_at = (app.plan_inline.get() && open_plan(app))
        .then(|| {
            ordered
                .iter()
                .filter(|item| undone_after.is_none_or(|boundary| item.sequence <= boundary))
                .rfind(|item| is_plan_update(item))
                .map(|item| item.id)
        })
        .flatten();
    for item in ordered {
        if undone_after.is_some_and(|boundary| item.sequence > boundary) {
            folded = true;
            continue;
        }
        // The sidebar owns TODO state. Hide both live tool activity and replayed
        // snapshots here so updates do not repeat the plan in the conversation.
        if is_plan_update(item) {
            if plan_at == Some(item.id) {
                let mut block = TaggedLines::default();
                block.push_tagged(plan_row(app, width), HitTarget::SidebarToggle);
                append_block(
                    &mut transcript,
                    block,
                    TranscriptGroup::Tool,
                    &mut previous_group,
                );
            }
            continue;
        }
        let group = transcript_group(item);
        let mut block = TaggedLines::default();
        match &item.data {
            ItemData::UserMessage {
                text, attachments, ..
            } => {
                if waiting.contains(&item.id) {
                    continue;
                }
                // A long paste reads as its opening lines until asked for;
                // the whole of it is still what was sent.
                let rows = user_block(text, width, theme);
                if rows.len() > USER_BLOCK_ROWS {
                    let expanded = app.expanded.contains(&item.id);
                    let shown = if expanded {
                        rows.len()
                    } else {
                        USER_PREVIEW_ROWS
                    };
                    let hidden = rows.len() - shown;
                    for row in rows.into_iter().take(shown) {
                        block.push_tagged(row, HitTarget::ToolToggle(item.id));
                    }
                    let toggle = if expanded {
                        "… click to collapse".to_owned()
                    } else {
                        format!("… +{hidden} more lines · click to expand")
                    };
                    let mut toggle_row = user_block(&toggle, width, theme);
                    for span in toggle_row
                        .iter_mut()
                        .flat_map(|row| row.spans.iter_mut().skip(1))
                    {
                        span.fg = theme.muted;
                    }
                    for row in toggle_row {
                        block.push_tagged(row, HitTarget::ToolToggle(item.id));
                    }
                } else {
                    block.extend(rows);
                }
                if !attachments.is_empty() {
                    let mut spans = vec![Span::new("  ", theme.muted)];
                    for (index, source) in attachments.iter().enumerate() {
                        let (badge, name) = crate::media::attachment_display(source);
                        if index > 0 {
                            spans.push(Span::new("   ", theme.muted));
                        }
                        spans.extend(crate::view::span::media_chip(badge, &name, theme));
                    }
                    block.push(Line::new(spans));
                }
            }
            ItemData::AssistantMessage { text } => {
                block.extend(app.cached_render(item.id, width, || {
                    let mut lines = render_markdown(text, width, theme, "  ");
                    mark_reply(&mut lines, theme);
                    lines
                }));
            }
            ItemData::Reasoning { text } if app.reasoning => {
                let mut rows = app.cached_render(item.id, width, || {
                    render_markdown(text, width, theme, REASONING_INDENT)
                });
                for row in &mut rows {
                    for span in &mut row.spans {
                        span.fg = theme.muted;
                    }
                }
                block.extend(rows);
            }
            ItemData::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } if !call_id.trim().is_empty() && !name.trim().is_empty() => {
                let result = paired_result(app, item, call_id);
                // A command that ran and exited non-zero failed too, and its
                // row says with which code.
                let exit_code = result.and_then(|item| match &item.data {
                    ItemData::ToolResult {
                        name,
                        is_error: false,
                        metadata,
                        ..
                    } => failed_exit(name, metadata),
                    _ => None,
                });
                // A batch succeeds as a call and reports its failed steps
                // in its metadata; any failed step fails the row.
                let steps = if name == "batch" {
                    batch_steps(app, arguments, result)
                } else {
                    Vec::new()
                };
                let failed_steps = batch_failures(name, result, &steps);
                let result_status = result.and_then(|item| match &item.data {
                    ItemData::ToolResult {
                        output, is_error, ..
                    } => Some(if *is_error && output.trim() == "operation cancelled" {
                        "cancelled"
                    } else if *is_error && rejected_by_user(output) {
                        "rejected"
                    } else if *is_error || exit_code.is_some() || failed_steps > 0 {
                        "failed"
                    } else {
                        "done"
                    }),
                    _ => None,
                });
                let child_state = (name == "task")
                    .then(|| app.children.get(call_id))
                    .flatten()
                    .map(|task| task.state.as_str());
                // Agents launched from inside a batch are this call's live
                // work: the batch row stays alive while any of them runs and
                // each child gets its own row below.
                let batch_children = batch_children(app, name, call_id);
                let batch_active = batch_children.iter().any(|(_, _, task)| task.is_active());
                let status = if batch_active {
                    None
                } else if let Some(state) = child_state {
                    // The launch acknowledgement is not child completion.
                    // Once a durable child exists, its lifecycle is the source
                    // of truth for this row, including active/background and
                    // permission-waiting states.
                    match state {
                        "done" => Some("done"),
                        "failed" => Some("failed"),
                        "cancelled" | "interrupted" => Some("cancelled"),
                        _ => None,
                    }
                } else {
                    // Invalid task calls never create a child link. Their
                    // paired error is nevertheless terminal and must not
                    // leave a spinner running forever.
                    result_status
                };
                // A batch whose children are running is live work, never a
                // call waiting its turn behind another tool; nor is one
                // whose own step (`<call id>:N`) is the running tool.
                let queued = name != "task"
                    && !batch_active
                    && status.is_none()
                    && app.phase.as_ref().is_some_and(|phase| {
                        phase.state == "tool"
                            && phase.call_id.as_deref().is_some_and(|active| {
                                active != call_id
                                    && crate::state::batch_parent_call(active) != Some(call_id)
                            })
                    });
                let color = child_state.map_or_else(
                    || match status {
                        Some("failed") => theme.danger,
                        Some(_) => theme.muted,
                        None if queued => theme.muted,
                        None => theme.accent,
                    },
                    |state| task_state_style(state, app.tick, theme).1,
                );
                // Running tools breathe: the dot becomes a spinner frame.
                let marker = child_state.map_or_else(
                    || match status {
                        None if queued => "  ○ ".into(),
                        None => format!("  {} ", spinner(app.tick)),
                        Some("failed") => "  ✗ ".into(),
                        Some("cancelled" | "rejected") => "  ⊘ ".into(),
                        Some(_) => "  ● ".into(),
                    },
                    |state| format!("  {} ", task_state_style(state, app.tick, theme).0),
                );
                // A request the user turned down is their decision, not a
                // failure: it reads as quietly as a cancel.
                let suffix = match status {
                    Some("failed") if failed_steps > 0 => format!(
                        "  {}",
                        app.language
                            .format("{count} failed", &[("count", &failed_steps.to_string())])
                    ),
                    Some("failed") => exit_code.map_or_else(
                        || format!("  {}", app.language.text("failed")),
                        |code| format!("  exit {code}"),
                    ),
                    Some(state @ ("cancelled" | "rejected")) => {
                        format!("  {}", app.language.text(state))
                    }
                    None if queued => "  queued".to_owned(),
                    _ => String::new(),
                };
                // Child-agent tasks are alive: show what the child is doing
                // right now, not just that it was spawned.
                // A delegation's live detail goes on its own line below the
                // row; the row itself stays a plain name.
                let progress = if batch_children.is_empty() {
                    String::new()
                } else {
                    let running = batch_children
                        .iter()
                        .filter(|(_, _, task)| task.is_active())
                        .count();
                    let total = batch_children.len();
                    let agents = if total == 1 { "agent" } else { "agents" };
                    if running > 0 {
                        format!("  ·  {running} of {total} {agents} running")
                    } else {
                        format!("  ·  {total} {agents}")
                    }
                };
                let compact_result = result.filter(|item| folds_into_call_row(app, &item.data));
                // Keep ordinary successful activity rows quiet: the tool name,
                // primary argument, and status dot already communicate what
                // happened, while the tagged row remains available to expand.
                // Semantic empty states and mutation file counts still carry
                // useful information that is not implied by the call itself.
                let disclosure = result.map_or_else(String::new, |item| {
                    // A task row already opens its durable child, where the
                    // complete response belongs.
                    if name == "task" {
                        return String::new();
                    }
                    let ItemData::ToolResult {
                        name,
                        output,
                        is_error,
                        metadata,
                        ..
                    } = &item.data
                    else {
                        return String::new();
                    };
                    if *is_error {
                        return String::new();
                    }
                    let output = displayed_output(name, output, metadata);
                    let (files, added, removed) =
                        app.cached_diff_stat(item.id, || file_diff_stat(metadata));
                    if files > 0 && compact_result.is_some() {
                        // The row already names the file; the line counts
                        // are what a reader cannot infer from it.
                        let stat = format!("+{added} −{removed}");
                        return if files == 1 {
                            format!("  ·  {stat}")
                        } else {
                            format!("  ·  {files} files  {stat}")
                        };
                    }
                    if let Some(label) = empty_result_label(output) {
                        return format!("  ·  {label}");
                    }
                    if compact_result.is_none() {
                        return String::new();
                    }
                    String::new()
                });
                // A browser call that ran in the person's own Chrome acts with
                // their sign-ins, which is worth a word on the row. Until the
                // result says where it ran, the call's own choice stands in.
                let chrome = name.starts_with("browser_")
                    && result.map_or_else(
                        || arguments.get("browser").and_then(serde_json::Value::as_str),
                        |item| match &item.data {
                            ItemData::ToolResult { metadata, .. } => {
                                metadata.get("browser").and_then(serde_json::Value::as_str)
                            }
                            _ => None,
                        },
                    ) == Some("chrome");
                let browser = if chrome {
                    format!("  ·  {}", app.language.text("Chrome"))
                } else {
                    String::new()
                };
                // The status suffix must survive truncation: a row that ends
                // in a lone "f" instead of "failed" reads as a rendering bug.
                // Long tool names (`browser_tabs_open`) take their own width.
                let lead = (marker.chars().count() + name.chars().count() + 1).max(12);
                let argument_budget = width
                    .saturating_sub(
                        lead + progress.chars().count()
                            + suffix.chars().count()
                            + browser.chars().count(),
                    )
                    .max(16);
                let argument = if name == "workflow" {
                    format!(" {}", workflow_argument(arguments))
                } else {
                    tool_target(app.language, name, arguments)
                        .map(|value| format!(" {}", single_line(&value, argument_budget)))
                        .unwrap_or_default()
                };
                let row = Line::new(vec![
                    Span::new(marker, color),
                    Span::new(name.clone(), theme.text),
                    Span::new(format!("{argument}{suffix}"), color),
                    Span::new(browser, theme.muted),
                    Span::new(progress, theme.muted),
                    Span::new(disclosure, theme.muted),
                ]);
                // A batch made only of delegations is an implementation
                // detail: the wrapper stays invisible and every
                // delegation is an ordinary task row of its own.
                let wrapper_hidden = !batch_children.is_empty() && batch_is_only_tasks(arguments);
                if wrapper_hidden {
                    // rows follow below
                } else if name == "task"
                    && let Some(task) = app.children.get(call_id)
                {
                    block.push_tagged(row, HitTarget::TaskOpen(call_id.clone()));
                    block.push_tagged(
                        delegation_detail_line(task, theme),
                        HitTarget::TaskOpen(call_id.clone()),
                    );
                } else if name == "workflow"
                    && let Some(run_id) = workflow_run_id(arguments, result)
                {
                    block.push_tagged(row, HitTarget::WorkflowOpen(run_id));
                } else if let Some(result) =
                    compact_result.or(result.filter(|_| status == Some("rejected")))
                {
                    block.push_tagged(row, HitTarget::ToolToggle(result.id));
                } else {
                    block.push(row);
                }
                // An MCP tool whose server pairs it with a page: the page
                // renders in Desktop, so the row says where to find it.
                if result.is_some_and(|item| has_mcp_app(&item.data)) {
                    let notice = app.language.text(
                        "This tool comes with an app; open the chat in Haarness Desktop to use it.",
                    );
                    block.extend(wrap_spans(
                        &[Span::new(notice, theme.muted)],
                        width,
                        "    ↳ ",
                        "      ",
                        theme.muted,
                    ));
                }
                // Every other step of a batch gets a line of its own, so the
                // row says what it is doing and which step failed.
                if !wrapper_hidden && !steps.is_empty() {
                    let delegated: Vec<usize> =
                        batch_children.iter().map(|(index, _, _)| *index).collect();
                    let listed: Vec<&BatchStep> = steps
                        .iter()
                        .filter(|step| {
                            !(same_tool(step.tool, "task") && delegated.contains(&step.index))
                        })
                        .collect();
                    let expanded = result.is_some_and(|item| app.expanded.contains(&item.id));
                    let shown = if expanded {
                        listed.len()
                    } else if listed.len() > BATCH_STEP_ROWS {
                        BATCH_STEP_ROWS - 1
                    } else {
                        listed.len()
                    };
                    // The step running now: the turn's status names it as
                    // `<call id>:N`, the batch's Nth step.
                    let running = app
                        .phase
                        .as_ref()
                        .filter(|phase| phase.state == "tool")
                        .and_then(|phase| phase.call_id.as_deref())
                        .and_then(|active| active.strip_prefix(call_id.as_str()))
                        .and_then(|rest| rest.strip_prefix(':'))
                        .and_then(|index| index.parse::<usize>().ok());
                    let mut push = |row: Line| match result {
                        Some(result) => block.push_tagged(row, HitTarget::ToolToggle(result.id)),
                        None => block.push(row),
                    };
                    for step in &listed[..shown] {
                        let (glyph, tone) = match (step.failed, status) {
                            (Some(true), _) => ("✗".to_owned(), theme.danger),
                            (Some(false), _) | (None, Some("done")) => {
                                ("●".to_owned(), theme.muted)
                            }
                            (None, Some(_)) => ("⊘".to_owned(), theme.muted),
                            (None, None) if running == Some(step.index) => {
                                (spinner(app.tick).to_string(), theme.accent)
                            }
                            // Steps run in order: one before the running step
                            // has had its turn; its outcome comes with the batch.
                            (None, None) if running.is_some_and(|running| step.index < running) => {
                                ("●".to_owned(), theme.muted)
                            }
                            (None, None) => ("○".to_owned(), theme.muted),
                        };
                        let target = if same_tool(step.tool, "task") {
                            Some(batch_task_label(arguments, step.index))
                        } else {
                            tool_target(app.language, step.tool, step.parameters)
                        };
                        let budget = width.saturating_sub(7 + step.tool.chars().count()).max(12);
                        push(Line::new(vec![
                            Span::new(format!("    {glyph} "), tone),
                            Span::new(step.tool.to_owned(), theme.text),
                            Span::new(
                                target.map_or_else(String::new, |target| {
                                    format!(" {}", single_line(&target, budget))
                                }),
                                tone,
                            ),
                        ]));
                    }
                    if listed.len() > shown {
                        let more = app.language.format(
                            "+{count} more",
                            &[("count", &(listed.len() - shown).to_string())],
                        );
                        push(line(format!("    … {more}"), theme.muted));
                    }
                }
                for (index, child_call_id, task) in &batch_children {
                    let (glyph, tone) = task_state_style(&task.state, app.tick, theme);
                    let label = batch_task_label(arguments, *index);
                    let child_row = Line::new(vec![
                        Span::new(format!("  {glyph} "), tone),
                        Span::new("task", theme.text),
                        Span::new(
                            format!(" {}", single_line(&label, width.saturating_sub(12).max(12))),
                            tone,
                        ),
                    ]);
                    block.push_tagged(child_row, HitTarget::TaskOpen(child_call_id.clone()));
                    block.push_tagged(
                        delegation_detail_line(task, theme),
                        HitTarget::TaskOpen(child_call_id.clone()),
                    );
                }
                if let Some(result) = result {
                    render_tool_result(app, &mut block, result, name, width);
                }
            }
            ItemData::ToolResult { call_id, name, .. } if !call_id.trim().is_empty() => {
                // Normal results are rendered beside the matching call above.
                // Preserve malformed/orphan history rather than dropping it.
                let has_call = paired_call(app, item, call_id).is_some();
                if !has_call {
                    render_tool_result(app, &mut block, item, name, width);
                }
            }
            ItemData::Plan { markdown } => {
                block.push(line("  plan", theme.primary));
                block.extend(app.cached_render(item.id, width, || {
                    render_markdown(markdown, width, theme, "    ")
                }));
            }
            ItemData::Review { findings } => {
                block.push(line("  review", theme.primary));
                for finding in findings {
                    let color = match finding.severity.as_str() {
                        "high" | "error" | "critical" => theme.danger,
                        "medium" | "warning" => theme.accent,
                        _ => theme.muted,
                    };
                    let location = match (&finding.file, finding.line) {
                        (Some(file), Some(line_number)) => format!("{file}:{line_number}  "),
                        (Some(file), None) => format!("{file}  "),
                        _ => String::new(),
                    };
                    block.push(line(
                        format!(
                            "    ● {location}{}",
                            single_line(&finding.explanation, width.saturating_sub(12))
                        ),
                        color,
                    ));
                }
            }
            ItemData::Compaction { summary, .. } => {
                block.push(line(
                    format!(
                        "  ── compacted · {}",
                        single_line(summary, width.saturating_sub(20))
                    ),
                    theme.muted,
                ));
            }
            ItemData::McpAction { server, action, .. } => {
                block.push(line(
                    format!(
                        "  ◆ {server} · {}",
                        single_line(action, width.saturating_sub(12))
                    ),
                    theme.muted,
                ));
            }
            ItemData::Question {
                question_id,
                prompt,
                choices,
                multiple,
                custom,
            } => {
                // The durable item records that a question was asked, while
                // the root-wide pending queue is authoritative for whether it
                // still needs an answer. Hide resolved requests just like
                // resolved permission prompts; otherwise answering only
                // removes focus and leaves a stale checklist in the chat,
                // including after transcript replay/reconnect.
                if !app
                    .pending_questions
                    .iter()
                    .any(|pending| pending.question_id == *question_id)
                {
                    continue;
                }
                render_question(
                    app,
                    &mut block,
                    QuestionView {
                        id: question_id,
                        prompt,
                        choices,
                        multiple: *multiple,
                        custom: *custom,
                    },
                    width,
                );
            }
            // The user's own stop is not a failure: a quiet marker, as a
            // stopped reply reads in Upstream.
            ItemData::Error { message, .. } if message.trim() == "operation cancelled" => {
                block.push(line(
                    format!("  · {}", app.language.text("interrupted")),
                    theme.muted,
                ));
            }
            ItemData::Error { message, .. } => {
                for (index, wrapped) in textwrap::wrap(message, width.saturating_sub(4).max(10))
                    .into_iter()
                    .enumerate()
                {
                    block.push(line(
                        format!("{}{}", if index == 0 { "  × " } else { "    " }, wrapped),
                        theme.danger,
                    ));
                }
            }
            ItemData::MediaOmitted {
                names,
                reason,
                guessed,
                ..
            } => {
                let notice = media_omitted_notice(app.language, names, *guessed);
                for (index, wrapped) in textwrap::wrap(&notice, width.saturating_sub(4).max(10))
                    .into_iter()
                    .enumerate()
                {
                    block.push(line(
                        format!("{}{}", if index == 0 { "  ! " } else { "    " }, wrapped),
                        theme.accent,
                    ));
                }
                if !reason.trim().is_empty() {
                    block.push(line(
                        format!(
                            "    {}",
                            single_line(reason, width.saturating_sub(8).max(10))
                        ),
                        theme.muted,
                    ));
                }
            }
            // Usage feeds the status bar totals instead of the transcript.
            // Permission prompts live on the docked approval panel above the
            // composer while pending; once answered, the tool call and its
            // result already tell the story.
            _ => {}
        }
        let visible_rows = block
            .lines
            .iter()
            .position(|row| !blank(row))
            .zip(block.lines.iter().rposition(|row| !blank(row)))
            .map_or(0, |(first, last)| last - first + 1);
        append_block(&mut transcript, block, group, &mut previous_group);
        if visible_rows > 0 && crate::messages::message_text(item).is_some() {
            transcript
                .messages
                .push((item.id, transcript.len() - visible_rows));
        }
    }
    if folded {
        let (_, steps) = app.undo_depth();
        let text = if steps == 1 {
            app.language
                .text("1 step undone · /redo to restore")
                .to_owned()
        } else {
            app.language.format(
                "{count} steps undone · /redo to restore",
                &[("count", &steps.to_string())],
            )
        };
        let mut block = TaggedLines::default();
        block.push(Line::new(vec![
            Span::new("  ↶ ", theme.muted),
            Span::new(single_line(&text, width.saturating_sub(4)), theme.muted),
        ]));
        append_block(
            &mut transcript,
            block,
            TranscriptGroup::System,
            &mut previous_group,
        );
    }
    // Child-agent questions are durable on the child thread, not duplicated
    // into the root transcript. The structured root queue still renders the
    // active prompt here so it can be answered without navigating away.
    if let Some(pending) = app.first_pending_question()
        && !app.items.iter().any(|item| {
            matches!(
                &item.data,
                ItemData::Question { question_id, .. }
                    if question_id == &pending.question_id
            )
        })
    {
        let mut block = TaggedLines::default();
        render_question(
            app,
            &mut block,
            QuestionView {
                id: &pending.question_id,
                prompt: &pending.prompt,
                choices: &pending.choices,
                multiple: pending.multiple,
                custom: pending.custom,
            },
            width,
        );
        append_block(
            &mut transcript,
            block,
            TranscriptGroup::Question,
            &mut previous_group,
        );
    }
    if app.reasoning && !app.reasoning_preview.is_empty() {
        let mut block = TaggedLines::default();
        let mut live = render_markdown(&app.reasoning_preview, width, theme, REASONING_INDENT);
        for row in &mut live {
            for span in &mut row.spans {
                span.fg = theme.muted;
            }
        }
        block.extend(live);
        append_block(
            &mut transcript,
            block,
            TranscriptGroup::Reasoning,
            &mut previous_group,
        );
    }
    if !app.preview.is_empty() {
        // Render every streaming snapshot through the same Markdown pipeline
        // as the durable assistant message. The cursor is appended afterwards
        // so it cannot change how a partial fence/emphasis token parses.
        let trailing_break = app.preview.ends_with('\n');
        let mut live = render_markdown(&app.preview, width, theme, "  ");
        if live.is_empty() || trailing_break {
            live.push(line("  ", theme.text));
        }
        mark_reply(&mut live, theme);
        if let Some(last) = live.last_mut() {
            if last.width() < width {
                last.spans.push(Span::new("▌", theme.primary));
            } else {
                live.push(line("  ▌", theme.primary));
            }
        }
        let mut block = TaggedLines::default();
        block.extend(live);
        append_block(
            &mut transcript,
            block,
            TranscriptGroup::Assistant,
            &mut previous_group,
        );
    }
    transcript
}
