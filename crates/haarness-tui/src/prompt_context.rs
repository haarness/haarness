//! Bounded file snapshots and explicit delegation references. Like Desktop,
//! serialization uses ordinary message text so queues and archives retain it.
use crate::{api::ApiClient, state::App};
use anyhow::{Context, Result, ensure};

pub(crate) fn range(query: &str) -> (&str, Option<(usize, usize)>) {
    let Some((path, suffix)) = query.rsplit_once(':') else {
        return (query, None);
    };
    let (start, end) = suffix.split_once('-').unwrap_or((suffix, suffix));
    match (start.parse::<usize>(), end.parse::<usize>()) {
        (Ok(start), Ok(end)) if start > 0 && end >= start => (path, Some((start, end))),
        _ => (query, None),
    }
}
pub(crate) fn snapshot(path: &str, content: &str, lines: Option<(usize, usize)>) -> Result<String> {
    let selected = if let Some((start, end)) = lines {
        let all = content.lines().collect::<Vec<_>>();
        ensure!(
            end <= all.len(),
            "Requested range exceeds the file's {} lines",
            all.len()
        );
        all[start - 1..end].join("\n")
    } else {
        content.into()
    };
    ensure!(
        selected.len() <= 128 * 1024,
        "File context exceeds 128 KiB; select a smaller :start-end range"
    );
    let fence = "`".repeat(
        selected
            .split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .max(2)
            + 1,
    );
    let label = if let Some((start, end)) = lines {
        format!("{path}:{start}-{end}")
    } else {
        path.into()
    };
    let label = label
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    Ok(format!(
        "<details>\n<summary>File context: {label} (snapshot)</summary>\n\n{fence}\n{selected}\n{fence}\n\n</details>"
    ))
}
/// One piece of a sent prompt: what the person typed, or a reference they
/// attached with `@` (a file snapshot, a folder listing or a delegation),
/// carried as the exact text the model receives.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Part<'a> {
    Text(&'a str),
    Reference { label: String, body: &'a str },
}

const DELEGATION: &str = "Delegate the relevant work from this message and attached context using the task tool with agent ";
const DELEGATION_END: &str = ". Keep normal permissions in effect.";

/// Split a prompt into typed text and the references the TUI, Desktop and
/// server expand `@` mentions into, so they can be shown as the chips they
/// were typed as while the model still receives the full text.
pub(crate) fn parts(text: &str) -> Vec<Part<'_>> {
    let mut parts = Vec::new();
    let mut typed = 0;
    let mut cursor = 0;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let next = [rest.find("<details>\n<summary>"), rest.find(DELEGATION)]
            .into_iter()
            .flatten()
            .min();
        let Some(offset) = next else {
            break;
        };
        let start = cursor + offset;
        match reference_at(&text[start..]) {
            Some((label, length)) => {
                if typed < start {
                    parts.push(Part::Text(&text[typed..start]));
                }
                parts.push(Part::Reference {
                    label,
                    body: &text[start..start + length],
                });
                typed = start + length;
                cursor = typed;
            }
            // Both markers start with an ASCII byte.
            None => cursor = start + 1,
        }
    }
    if typed < text.len() {
        parts.push(Part::Text(&text[typed..]));
    }
    parts
}

/// The chip label and byte length of the reference `text` starts with.
fn reference_at(text: &str) -> Option<(String, usize)> {
    if let Some(rest) = text.strip_prefix(DELEGATION) {
        let mut values = serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let name = values.next()?.ok()?;
        let after = values.byte_offset();
        rest[after..].starts_with(DELEGATION_END).then(|| {
            (
                format!("@{name}"),
                DELEGATION.len() + after + DELEGATION_END.len(),
            )
        })
    } else {
        let rest = text.strip_prefix("<details>\n<summary>")?;
        let (summary, body) = rest.split_once("</summary>\n\n")?;
        let label = ["File context: ", "Directory listing: "]
            .iter()
            .find_map(|kind| summary.strip_prefix(kind))?;
        let label = [" (snapshot)", " (unsaved editor snapshot)"]
            .iter()
            .find_map(|suffix| label.strip_suffix(suffix))
            .unwrap_or(label);
        // A snapshot's fence is longer than any backtick run inside it, so
        // the first matching fence closes it.
        let end = match body.split_once('\n') {
            Some((fence, _)) if fence.len() >= 3 && fence.bytes().all(|b| b == b'`') => {
                let close = format!("\n{fence}\n\n</details>");
                body[fence.len()..].find(&close)? + fence.len() + close.len()
            }
            _ => body.find("\n\n</details>")? + "\n\n</details>".len(),
        };
        let label = label
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        Some((
            format!("@{label}"),
            text.len() - rest.len() + summary.len() + "</summary>\n\n".len() + end,
        ))
    }
}

/// A sent prompt as the person typed it, references back to `@` chips.
pub(crate) fn display(text: &str) -> std::borrow::Cow<'_, str> {
    let parts = parts(text);
    if parts.iter().all(|part| matches!(part, Part::Text(_))) {
        return std::borrow::Cow::Borrowed(text);
    }
    parts
        .into_iter()
        .map(|part| match part {
            Part::Text(text) => std::borrow::Cow::Borrowed(text),
            Part::Reference { label, .. } => std::borrow::Cow::Owned(label),
        })
        .collect::<String>()
        .into()
}

pub(crate) async fn apply(app: &mut App, client: &ApiClient) -> Result<bool> {
    let Some((start, query)) = app.composer.mention_query() else {
        return Ok(false);
    };
    let Some(selected) = app.mention_suggestions.get(app.selection).cloned() else {
        return Ok(false);
    };
    if selected.ends_with('/')
        || selected.starts_with("../")
        || selected.starts_with("~/")
        || std::path::Path::new(&selected).is_absolute()
    {
        return Ok(crate::input::apply_mention(app));
    }
    let end = start + 1 + query.len();
    let (label, content) = if let Some(name) = selected.strip_prefix("agent://") {
        ensure!(
            app.agents.iter().any(|a| a.id == name
                && !a.hidden
                && matches!(a.mode.as_deref(), Some("subagent" | "all"))),
            "Agent is not available for delegation"
        );
        (
            format!("@{name}"),
            format!(
                "Delegate the relevant work from this message and attached context using the task tool with agent {}. Keep normal permissions in effect.",
                serde_json::to_string(name)?
            ),
        )
    } else {
        let (path, lines) = range(&selected);
        let file = client.file_content(path).await?;
        let content = file["content"]
            .as_str()
            .context("File content unavailable")?;
        if file["encoding"] == "base64" {
            ensure!(lines.is_none(), "Line ranges require a text file");
            let mime = file["mime"]
                .as_str()
                .context("File media type unavailable")?;
            ensure!(
                mime != "application/octet-stream",
                "This binary format cannot be attached"
            );
            app.add_attachments(vec![format!("data:{mime};base64,{content}")]);
            app.composer.replace_range(start, end, "");
            app.mention_suggestions.clear();
            return Ok(true);
        }
        (format!("@{selected}"), snapshot(path, content, lines)?)
    };
    app.composer.attach_context(start, end, &label, &content)?;
    app.mention_suggestions.clear();
    app.selection = 0;
    app.notice = format!("Attached {label} · Backspace removes the reference");
    Ok(true)
}
