use anyhow::{Context, Result};
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
    // Plain `cargo metadata` JSON: the cargo_metadata crate turns on extra
    // serde_json and semver features, which feature unification would apply
    // to the whole workspace, so `cargo test --workspace` would rebuild and
    // relink haarness with JSON parsing the shipped binary does not have.
    let output =
        std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .output()
            .context("run cargo metadata")?;
    anyhow::ensure!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
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
        let found = metadata["packages"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|candidate| candidate["name"] == package)
            .with_context(|| format!("missing package {package}"))?;
        let dependencies: BTreeSet<_> = found["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|dependency| dependency["name"].as_str())
            .collect();
        for dependency in forbidden {
            if dependencies.contains(dependency) {
                anyhow::bail!("architecture violation: {package} depends on {dependency}");
            }
        }
    }
    let root = std::path::Path::new(
        metadata["workspace_root"]
            .as_str()
            .context("cargo metadata has no workspace root")?,
    );
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
