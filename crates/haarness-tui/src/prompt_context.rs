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
        (selected.clone(), snapshot(path, content, lines)?)
    };
    app.composer.attach_context(start, end, &label, &content)?;
    app.mention_suggestions.clear();
    app.selection = 0;
    app.notice = format!("Attached {label} · Backspace removes the reference");
    Ok(true)
}
