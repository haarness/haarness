use anyhow::{Context as _, Result};
use clap::Args;
use haarness_insights::{Options, collect, render_html, render_text, write_report};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct Filters {
    /// Rolling UTC window, selected by turn start time (default: all history).
    #[arg(long)]
    days: Option<u32>,
    /// Registered project ID, unique name, or root path.
    #[arg(long, conflicts_with = "here")]
    project: Option<String>,
    /// Only this project: every workspace of the project you are in.
    #[arg(long)]
    here: bool,
    /// Include at most this many recent matching chats (default: no limit).
    #[arg(long)]
    limit: Option<usize>,
}
impl Filters {
    fn options(&self) -> Options {
        Options {
            days: self.days,
            project: self.project.clone(),
            current_project: self.here,
            limit: self.limit,
        }
    }
}
#[derive(Args)]
pub struct StatsArgs {
    #[command(flatten)]
    filters: Filters,
    /// Emit the full versioned report as JSON.
    #[arg(long)]
    json: bool,
}
#[derive(Args)]
// Independent CLI switches retain compatibility without changing the report's execution mode.
#[allow(clippy::struct_excessive_bools)]
pub struct InsightsArgs {
    #[command(flatten)]
    filters: Filters,
    /// Write the unique report into this directory.
    #[arg(long)]
    output_dir: Option<PathBuf>,
    /// Open the report in the system browser after generation.
    #[arg(long, conflicts_with = "no_open")]
    open: bool,
    /// Print the path without opening a browser (the default).
    #[arg(long)]
    no_open: bool,
    /// Reports are deterministic and never call an LLM; accepted for compatibility.
    #[arg(long)]
    no_llm: bool,
    /// Emit the report path and full statistics as JSON.
    #[arg(long)]
    json: bool,
}
pub fn stats(data: &Path, workspace: &Path, args: &StatsArgs) -> Result<()> {
    let report = collect(data, workspace, args.filters.options())?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render_text(&report));
    }
    Ok(())
}
pub fn insights(data: &Path, workspace: &Path, args: &InsightsArgs) -> Result<()> {
    let report = collect(data, workspace, args.filters.options())?;
    let directory = args
        .output_dir
        .clone()
        .unwrap_or_else(|| data.join("insights/reports"));
    let path = dunce::canonicalize(&write_report(&directory, &render_html(&report))?)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"path":path,"report":report}))?
        );
    } else {
        print!("{}\nReport: {}\n", render_text(&report), path.display());
    }
    if args.open {
        let mut command = if cfg!(target_os = "macos") {
            std::process::Command::new("open")
        } else if cfg!(target_os = "windows") {
            let mut c = std::process::Command::new("rundll32");
            c.arg("url.dll,FileProtocolHandler");
            c
        } else {
            std::process::Command::new("xdg-open")
        };
        let status = command
            .arg(&path)
            .status()
            .context("open generated report")?;
        if !status.success() {
            anyhow::bail!(
                "report saved at {}, but the browser opener failed",
                path.display()
            );
        }
    }
    Ok(())
}
