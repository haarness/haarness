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
        default_value = "https://haarness.ai"
    )]
    release_url: String,
}
impl UpgradeArgs {
    /// The newest stable release, from `HAARNESS_RELEASE_URL` or haarness.ai:
    /// what `haarness --update` installs.
    pub fn latest() -> Self {
        Self {
            channel: "stable".into(),
            version: "latest".into(),
            release_url: std::env::var("HAARNESS_RELEASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "https://haarness.ai".into()),
        }
    }
}
/// A `haarness` in Desktop's `resources/bin` (`Resources/bin` in the macOS
/// app) is part of Desktop: replacing it would break the app's signature, and
/// Desktop updates it with itself.
fn bundled_with_desktop(directory: &std::path::Path) -> bool {
    directory
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
        && directory
            .parent()
            .and_then(std::path::Path::file_name)
            .is_some_and(|name| name.eq_ignore_ascii_case("resources"))
}
pub fn run(args: UpgradeArgs) -> Result<()> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let directory = executable
        .parent()
        .context("Cannot determine installation directory")?;
    if bundled_with_desktop(directory) {
        bail!(
            "This haarness comes with Haarness Desktop, which keeps it up to date: update Desktop instead (Settings → About), or install the standalone CLI with `curl -fsSL https://haarness.ai/install.sh | HAARNESS_PRODUCT=cli sh`"
        );
    }
    #[cfg(unix)]
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(include_str!("../../../install.sh"))
        .env("HAARNESS_INSTALL_DIR", dunce::simplified(directory))
        // Upgrading in place: the folder is already where the user runs it from.
        .env("HAARNESS_NO_MODIFY_PATH", "1")
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
        .env("HAARNESS_INSTALL_DIR", dunce::simplified(directory))
        // Upgrading in place: the folder is already where the user runs it from.
        .env("HAARNESS_NO_MODIFY_PATH", "1")
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
