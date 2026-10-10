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

/// `12642` as `12,642`.
pub(crate) fn count(value: impl Into<u64>) -> String {
    let digits = value.into().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// A count and its noun: `1 project`, `12,642 turns`.
pub(crate) fn noun(value: impl Into<u64>, one: &str, many: &str) -> String {
    let value = value.into();
    format!("{} {}", count(value), if value == 1 { one } else { many })
}

/// Dollars to the cent; an amount under a cent keeps four decimals so it
/// does not read as free.
pub(crate) fn money(usd: f64) -> String {
    if usd > 0.0 && usd < 0.01 {
        return format!("${usd:.4}");
    }
    let cents = format!("{usd:.2}");
    let (dollars, cents) = cents.split_once('.').unwrap_or((&cents, "00"));
    format!("${}.{cents}", count(dollars.parse::<u64>().unwrap_or(0)))
}

pub fn render_text(report: &Report) -> String {
    let m = &report.total;
    let mut text = format!(
        "Haarness · Usage stats\n{} · {} · {}\n{} input + {} output tokens · cache read {} / write {}\nKnown cost {} · {} · {} without usage\n{} · {} · {} (+{} / -{})\n",
        noun(report.projects.len() as u64, "project", "projects"),
        noun(m.sessions, "chat", "chats"),
        noun(m.turns, "turn", "turns"),
        count(m.input_tokens),
        count(m.output_tokens),
        count(m.cache_read_tokens),
        count(m.cache_write_tokens),
        money(m.known_cost_usd),
        noun(
            m.unpriced_usage_records,
            "unpriced usage record",
            "unpriced usage records"
        ),
        noun(m.turns_without_usage, "turn", "turns"),
        noun(m.tool_calls, "tool call", "tool calls"),
        noun(m.tool_errors, "tool error", "tool errors"),
        noun(m.file_changes, "file change", "file changes"),
        count(m.additions),
        count(m.deletions)
    );
    for (title, groups) in [
        ("Projects", &report.projects),
        ("Models", &report.models),
        ("Tools", &report.tools),
    ] {
        let _ = writeln!(text, "\n{title}");
        if groups.is_empty() {
            let _ = writeln!(text, "  none");
        }
        for group in groups {
            let m = &group.metrics;
            let _ = writeln!(
                text,
                "  {} · {} · {} · {} known · {} / {}",
                terminal_text(&group.name),
                noun(m.turns, "turn", "turns"),
                noun(m.tokens(), "token", "tokens"),
                money(m.known_cost_usd),
                noun(m.tool_calls, "call", "calls"),
                noun(m.tool_errors, "error", "errors")
            );
        }
    }
    if !report.findings.is_empty() {
        let _ = writeln!(text, "\nFindings");
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
        "{selection} · {scope} · {} · {} of {}",
        noun(report.projects.len() as u64, "project", "projects"),
        count(m.sessions),
        noun(
            report.matching_sessions as u64,
            "matching chat",
            "matching chats"
        )
    );
    html = html
        .replace("{{scope}}", &escape(&scope))
        .replace("{{generated}}", &report.generated_at.to_rfc3339());
    let mut cards = String::new();
    for (label, value, detail) in [
        (
            "Chats",
            count(m.sessions),
            format!(
                "{} · {} completed",
                noun(m.turns, "turn", "turns"),
                count(m.completed)
            ),
        ),
        (
            "Input + output tokens",
            count(m.tokens()),
            format!(
                "{} input · {} output",
                count(m.input_tokens),
                count(m.output_tokens)
            ),
        ),
        (
            "Known cost",
            money(m.known_cost_usd),
            format!(
                "{} · {} unpriced",
                noun(m.priced_usage_records, "priced record", "priced records"),
                count(m.unpriced_usage_records)
            ),
        ),
        (
            "Tool calls",
            count(m.tool_calls),
            format!(
                "{} · {}",
                noun(m.tool_errors, "error", "errors"),
                noun(m.tool_results, "result", "results")
            ),
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
        ("sessions", "Chats", &report.sessions),
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
        count(m.cache_read_tokens),
        count(m.cache_write_tokens)
    );
    let _ = write!(
        warnings,
        "<p>{} and {} excluded. {} no usage record.</p>",
        noun(
            report.duplicate_turns_skipped as u64,
            "duplicate imported turn",
            "duplicate imported turns"
        ),
        noun(
            report.detached_items_skipped,
            "detached context item",
            "detached context items"
        ),
        if m.turns_without_usage == 1 {
            "1 turn has".to_owned()
        } else {
            format!("{} turns have", count(m.turns_without_usage))
        }
    );
    html.replace("{{coverage}}", &warnings)
}

fn section(html: &mut String, id: &str, title: &str, groups: &[Group]) {
    let _ = write!(
        html,
        "<section id={id}><div class=section-title><h2>{title}</h2><span>{}</span></div>",
        noun(groups.len() as u64, "entry", "entries")
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
                count(m.tool_calls),
                count(m.tool_results),
                count(m.tool_errors),
            ]
        } else if id == "files" {
            vec![
                count(m.file_changes),
                format!("+{}", count(m.additions)),
                format!("−{}", count(m.deletions)),
            ]
        } else {
            vec![
                count(m.turns),
                count(m.tokens()),
                money(m.known_cost_usd),
                count(m.unpriced_usage_records),
                count(m.tool_errors),
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

#[cfg(test)]
mod tests {
    use super::{count, money, noun};

    #[test]
    fn numbers_read_like_numbers() {
        assert_eq!(count(0u64), "0");
        assert_eq!(count(999u64), "999");
        assert_eq!(count(12_642u64), "12,642");
        assert_eq!(count(1_234_567u64), "1,234,567");
        assert_eq!(noun(1u64, "project", "projects"), "1 project");
        assert_eq!(noun(2_000u64, "turn", "turns"), "2,000 turns");
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(money(0.004_2), "$0.0042");
        assert_eq!(money(0.01), "$0.01");
        assert_eq!(money(1_234.567), "$1,234.57");
    }
}
