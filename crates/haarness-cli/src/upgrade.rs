use anyhow::{Context, Result, bail};
use clap::Args;
#[derive(Args)]
pub struct UpgradeArgs {
    /// Release channel: stable or preview.
    #[arg(long, default_value="stable", value_parser=["stable", "preview"])]
    channel: String,
    /// Version to install.
    #[arg(long, default_value = "latest")]
    version: String,
    /// Release service origin (HTTPS; loopback HTTP for local testing).
    #[arg(
        long,
        env = "HAARNESS_RELEASE_URL",
        default_value = "https://app.haarness.ai"
    )]
    release_url: String,
}
pub fn run(args: UpgradeArgs) -> Result<()> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let directory = executable
        .parent()
        .context("Cannot determine installation directory")?;
    #[cfg(unix)]
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(include_str!("../../../install.sh"))
        .env("HAARNESS_INSTALL_DIR", directory)
        .env("HAARNESS_PRODUCT", "cli")
        .env("HAARNESS_CHANNEL", args.channel)
        .env("HAARNESS_VERSION", args.version)
        .env("HAARNESS_RELEASE_URL", args.release_url)
        .status()?;
    #[cfg(windows)]
    let status = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            include_str!("../../../install.ps1"),
        ])
        .env("HAARNESS_INSTALL_DIR", directory)
        .env("HAARNESS_PRODUCT", "cli")
        .env("HAARNESS_CHANNEL", args.channel)
        .env("HAARNESS_VERSION", args.version)
        .env("HAARNESS_RELEASE_URL", args.release_url)
        .status()?;
    if !status.success() {
        bail!("Upgrade failed; see installer output above");
    }
    Ok(())
}
