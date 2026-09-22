use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use clap::{Parser, Subcommand};
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Architecture,
    Package { binary: PathBuf },
}
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Architecture => architecture(),
        Command::Package { binary } => package(&binary),
    }
}

fn architecture() -> Result<()> {
    let metadata = MetadataCommand::new().no_deps().exec()?;
    let rules = [
        (
            "haarness-core",
            [
                "axum",
                "rusqlite",
                "reqwest",
                "rmcp",
                "clap",
                "crossterm",
                "haarness-opentui",
            ]
            .as_slice(),
        ),
        (
            "haarness-protocol",
            [
                "axum",
                "rusqlite",
                "reqwest",
                "rmcp",
                "clap",
                "crossterm",
                "haarness-core",
            ]
            .as_slice(),
        ),
        (
            "haarness-tui",
            [
                "haarness-core",
                "haarness-store",
                "haarness-providers",
                "haarness-mcp",
                "haarness-server",
            ]
            .as_slice(),
        ),
    ];
    for (package, forbidden) in rules {
        let found = metadata
            .packages
            .iter()
            .find(|candidate| candidate.name.as_str() == package)
            .with_context(|| format!("missing package {package}"))?;
        let dependencies: BTreeSet<_> = found
            .dependencies
            .iter()
            .map(|dependency| dependency.name.as_str())
            .collect();
        for dependency in forbidden {
            if dependencies.contains(dependency) {
                anyhow::bail!("architecture violation: {package} depends on {dependency}");
            }
        }
    }
    let root = metadata.workspace_root.as_std_path();
    for entry in walkdir::WalkDir::new(root.join("crates"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "rs")
        })
    {
        if entry
            .path()
            .components()
            .any(|component| component.as_os_str() == "haarness-opentui")
        {
            continue;
        }
        let body = std::fs::read_to_string(entry.path())?;
        if body.contains("unsafe {") || body.contains("unsafe fn") {
            anyhow::bail!(
                "unsafe Rust outside haarness-opentui: {}",
                entry.path().display()
            );
        }
    }
    println!("architecture boundaries: ok");
    Ok(())
}
fn package(binary: &PathBuf) -> Result<()> {
    let metadata = std::fs::metadata(binary)
        .with_context(|| format!("missing binary {}", binary.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("package target is not a single regular file");
    }
    if metadata.len() < 1_000_000 {
        anyhow::bail!("binary is unexpectedly small; embedded OpenTUI archive may be absent");
    }
    println!(
        "single executable: {} ({} bytes)",
        binary.display(),
        metadata.len()
    );
    Ok(())
}
