use crate::{Group, Report};
use anyhow::Result;
use std::{
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
};

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn render_text(report: &Report) -> String {
    let m = &report.total;
    let mut text = format!(
        "Haarness · Cross-project insights\n{} projects · {} sessions · {} turns\n{} input + {} output tokens · cache read {} / write {}\nKnown cost ${:.4} · {} unpriced usage records · {} turns without usage\n{} tool calls · {} tool errors · {} file changes (+{} / -{})\n",
        report.projects.len(),
        m.sessions,
        m.turns,
        m.input_tokens,
        m.output_tokens,
        m.cache_read_tokens,
        m.cache_write_tokens,
        m.known_cost_usd,
        m.unpriced_usage_records,
        m.turns_without_usage,
        m.tool_calls,
        m.tool_errors,
        m.file_changes,
        m.additions,
        m.deletions
    );
    for (title, groups) in [
        ("Projects", &report.projects),
        ("Models", &report.models),
        ("Tools", &report.tools),
    ] {
        let _ = writeln!(text, "\n{title}");
        for group in groups {
            let m = &group.metrics;
            let _ = writeln!(
                text,
                "  {} · {} turns · {} tokens · ${:.4} known · {} calls / {} errors",
                terminal_text(&group.name),
                m.turns,
                m.tokens(),
                m.known_cost_usd,
                m.tool_calls,
                m.tool_errors
            );
        }
    }
    for finding in &report.findings {
        let _ = writeln!(text, "\n• {}", terminal_text(finding));
    }
    for warning in &report.warnings {
        let _ = writeln!(text, "WARNING: {}", terminal_text(warning));
    }
    text
}

fn terminal_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn render_html(report: &Report) -> String {
    let mut html = String::from(include_str!("report.html"));
    let m = &report.total;
    let scope = report.since.map_or_else(
        || "All recorded history".into(),
        |start| {
            format!(
                "{} → {} · UTC",
                start.format("%d %b %Y %H:%M"),
                report.generated_at.format("%d %b %Y %H:%M")
            )
        },
    );
    let selection = if report.options.current_project {
        "Current project".to_owned()
    } else {
        report
            .options
            .project
            .as_ref()
            .map_or_else(|| "All projects".to_owned(), |p| format!("Project: {p}"))
    };
    let scope = format!(
        "{selection} · {scope} · {} projects · {} databases · {} of {} matching sessions",
        report.projects.len(),
        report.databases_read,
        m.sessions,
        report.matching_sessions
    );
    html = html
        .replace("{{scope}}", &escape(&scope))
        .replace("{{generated}}", &report.generated_at.to_rfc3339());
    let mut cards = String::new();
    for (label, value, detail) in [
        (
            "Sessions",
            m.sessions.to_string(),
            format!("{} turns · {} completed", m.turns, m.completed),
        ),
        (
            "Input + output tokens",
            m.tokens().to_string(),
            format!("{} input · {} output", m.input_tokens, m.output_tokens),
        ),
        (
            "Known cost",
            format!("${:.4}", m.known_cost_usd),
            format!(
                "{} priced · {} unpriced records",
                m.priced_usage_records, m.unpriced_usage_records
            ),
        ),
        (
            "Tool calls",
            m.tool_calls.to_string(),
            format!("{} errors · {} results", m.tool_errors, m.tool_results),
        ),
    ] {
        let _ = write!(
            cards,
            "<article class=card><span>{label}</span><strong>{value}</strong><small>{detail}</small></article>"
        );
    }
    html = html.replace("{{cards}}", &cards);
    let mut findings = String::new();
    for (index, finding) in report.findings.iter().enumerate() {
        let _ = write!(
            findings,
            "<article class=finding><span class=number>{:02}</span><p>{}</p></article>",
            index + 1,
            escape(finding)
        );
    }
    html = html.replace("{{findings}}", &findings);
    let mut sections = String::new();
    for (id, title, groups) in [
        ("projects", "Projects", &report.projects),
        ("models", "Models", &report.models),
        ("tools", "Tools", &report.tools),
        ("files", "Recorded file changes", &report.files),
        ("days", "Daily activity · UTC", &report.days),
        ("sessions", "Sessions", &report.sessions),
    ] {
        section(&mut sections, id, title, groups);
    }
    html = html.replace("{{sections}}", &sections);
    let mut warnings = String::new();
    for warning in &report.warnings {
        let _ = write!(warnings, "<p class=warning>{}</p>", escape(warning));
    }
    if report.warnings.is_empty() {
        warnings.push_str("<p>All discovered databases were read successfully.</p>");
    }
    let _ = write!(
        warnings,
        "<p>Reported cache tokens: {} read · {} written (separate from the input/output total).</p>",
        m.cache_read_tokens, m.cache_write_tokens
    );
    let _ = write!(
        warnings,
        "<p>{} duplicate imported turns and {} detached context items excluded. {} turns have no usage record.</p>",
        report.duplicate_turns_skipped, report.detached_items_skipped, m.turns_without_usage
    );
    html.replace("{{coverage}}", &warnings)
}

fn section(html: &mut String, id: &str, title: &str, groups: &[Group]) {
    let _ = write!(
        html,
        "<section id={id}><div class=section-title><h2>{title}</h2><span>{} entries</span></div>",
        groups.len()
    );
    if groups.is_empty() {
        html.push_str("<p class=empty>No recorded activity in this scope.</p></section>");
        return;
    }
    html.push_str("<div class=table-wrap><table><thead><tr><th>Name</th>");
    let headers: &[&str] = if id == "tools" {
        &["Calls", "Results", "Errors"]
    } else if id == "files" {
        &["Changes", "Added", "Removed"]
    } else {
        &["Turns", "Tokens", "Known cost", "Unpriced", "Tool errors"]
    };
    for header in headers {
        let _ = write!(html, "<th>{header}</th>");
    }
    html.push_str("</tr></thead><tbody>");
    for group in groups {
        let m = &group.metrics;
        let _ = write!(
            html,
            "<tr><td><span>{}</span><small>{}</small></td>",
            escape(&group.name),
            escape(&group.id)
        );
        let values = if id == "tools" {
            vec![
                m.tool_calls.to_string(),
                m.tool_results.to_string(),
                m.tool_errors.to_string(),
            ]
        } else if id == "files" {
            vec![
                m.file_changes.to_string(),
                format!("+{}", m.additions),
                format!("−{}", m.deletions),
            ]
        } else {
            vec![
                m.turns.to_string(),
                m.tokens().to_string(),
                format!("${:.4}", m.known_cost_usd),
                m.unpriced_usage_records.to_string(),
                m.tool_errors.to_string(),
            ]
        };
        for value in values {
            let _ = write!(html, "<td>{value}</td>");
        }
        html.push_str("</tr>");
    }
    html.push_str("</tbody></table></div></section>");
}

/// Create a private, unique file (never follow an existing symlink or overwrite a report).
pub fn write_report(directory: &Path, html: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    let path = directory.join(format!(
        "insights-{}-{}.html",
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        uuid::Uuid::now_v7()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(html.as_bytes())?;
    file.sync_all()?;
    Ok(path)
}
