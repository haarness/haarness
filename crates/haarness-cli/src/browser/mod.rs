//! Haarness for Chrome on this computer: `haarness browser setup|status|remove`
//! and the hidden `native-host` bridge Chrome starts for the extension.
//!
//! Discovery, relay and file staging are transport-agnostic; framing and the
//! host manifests are Chromium's.
pub(crate) mod discovery;
mod embedded;
mod files;
mod framing;
mod manifest;
pub(crate) mod native_host;
mod relay;
mod setup;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// The native messaging host name Chrome looks up.
pub(crate) const HOST_NAME: &str = "ai.haarness.browser";
/// Haarness for Chrome's ID, pinned by the `key` in its manifest.
pub(crate) const EXTENSION_ID: &str = "accdjhmcgljbadpeanajllmcembhkomn";
/// Where Haarness for Chrome is published. Until it is, people load the
/// unpacked copy setup puts in the data folder.
pub(crate) const CHROME_WEB_STORE_URL: Option<&str> = None;
/// Advertised by servers that accept an extension host.
pub(crate) const EXTENSION_CAPABILITY: &str = "browser-extension.v1";
/// The bridge ↔ extension protocol.
pub(crate) const PROTOCOL: u64 = 1;
/// The unpacked extension copy's folder name, which people see in Chrome's picker.
pub(crate) const EXTENSION_FOLDER: &str = "Haarness for Chrome";

#[derive(Args)]
pub struct BrowserArgs {
    #[command(subcommand)]
    command: BrowserCommand,
}

#[derive(Subcommand)]
enum BrowserCommand {
    /// Let Chrome start Haarness, and prepare Haarness for Chrome to load.
    Setup(SetupArgs),
    /// Show whether Chrome can start Haarness and which browsers are connected.
    Status {
        /// Print the status as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop Chrome from starting Haarness.
    Remove {
        /// Also delete the copy of Haarness for Chrome.
        #[arg(long)]
        all: bool,
        /// Print what was removed as JSON.
        #[arg(long)]
        json: bool,
    },
    /// The bridge Chrome starts for Haarness for Chrome. Not for people.
    #[command(hide = true)]
    NativeHost {
        /// The calling extension's origin, as Chrome passes it.
        origin: Option<String>,
        /// The calling window on Windows, as Chrome passes it.
        #[arg(long)]
        parent_window: Option<String>,
    },
}

#[derive(Args)]
#[allow(clippy::struct_excessive_bools)] // independent CLI switches validated by clap
pub(crate) struct SetupArgs {
    /// Register only with this browser (repeatable). Default: Google Chrome
    /// and every other supported browser installed here.
    #[arg(long = "browser", value_enum, value_name = "BROWSER")]
    pub(crate) browsers: Vec<BrowserChoice>,
    /// Write the host manifest only into this folder (repeatable), such as a
    /// test profile's NativeMessagingHosts.
    #[arg(long = "manifest-dir", value_name = "DIR")]
    pub(crate) manifest_dirs: Vec<PathBuf>,
    /// Print the result as JSON.
    #[arg(long)]
    pub(crate) json: bool,
    /// Print nothing but errors (and the JSON, with --json).
    #[arg(long)]
    pub(crate) quiet: bool,
    /// Change nothing that already works; respect an earlier `browser remove`.
    #[arg(long)]
    pub(crate) if_missing: bool,
    /// With a separate data folder (--data-dir, HAARNESS_DATA_DIR or
    /// HAARNESS_HOME) and no --manifest-dir: register it with your own
    /// browsers anyway, so your Chrome starts Haarness from that folder.
    #[arg(long)]
    pub(crate) use_isolated_data_for_chrome: bool,
}

/// Browsers setup can register with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum BrowserChoice {
    Chrome,
    ChromeBeta,
    ChromeCanary,
    ChromeForTesting,
    Chromium,
    Edge,
    Brave,
    Vivaldi,
}

/// `setup`, `status` and `remove`. `data` and `cache` are the resolved data
/// planes; `data_dir` is `--data-dir`/`HAARNESS_DATA_DIR` when one was given.
pub(crate) async fn run(
    data: &Path,
    cache: &Path,
    data_dir: Option<&Path>,
    args: &BrowserArgs,
) -> Result<()> {
    match &args.command {
        BrowserCommand::Setup(args) => setup::setup_command(data, data_dir, args),
        BrowserCommand::Status { json } => setup::status_command(data, cache, *json).await,
        BrowserCommand::Remove { all, json } => setup::remove_command(data, *all, *json),
        // Reached only through global options before `browser`; the bare
        // forms never get this far (see `native_host_requested`).
        BrowserCommand::NativeHost { .. } => native_host::main().await,
    }
}

/// Chrome starts a host as `<path> chrome-extension://<id>/ [--parent-window=N]`;
/// the wrapper turns that into `haarness browser native-host …`. Either form is
/// the bridge, recognised before anything parses arguments, touches the data
/// folder or writes to stdout.
pub(crate) fn native_host_requested(args: &[OsString]) -> bool {
    let word = |index: usize| args.get(index).and_then(|value| value.to_str());
    // Chrome never asks for help, so `browser native-host --help` is a person
    // reading it: clap prints the help instead of starting the bridge.
    let help = args
        .iter()
        .skip(3)
        .any(|arg| arg == "--help" || arg == "-h");
    word(1).is_some_and(|first| first.starts_with("chrome-extension://"))
        || (word(1) == Some("browser") && word(2) == Some("native-host") && !help)
}

/// One line for `haarness doctor`. Never fails.
pub(crate) fn doctor_line(data: &Path) -> String {
    setup::doctor_summary(data)
}

#[cfg(test)]
mod tests {
    use super::native_host_requested;
    use std::ffi::OsString;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn chrome_starts_the_bridge_with_either_argument_form() {
        assert!(native_host_requested(&args(&[
            "haarness",
            "chrome-extension://accdjhmcgljbadpeanajllmcembhkomn/"
        ])));
        assert!(native_host_requested(&args(&[
            "haarness",
            "chrome-extension://accdjhmcgljbadpeanajllmcembhkomn/",
            "--parent-window=0"
        ])));
        assert!(native_host_requested(&args(&[
            "haarness",
            "browser",
            "native-host",
            "chrome-extension://accdjhmcgljbadpeanajllmcembhkomn/"
        ])));
        for other in [
            &["haarness"][..],
            &["haarness", "browser", "status"],
            &["haarness", "browser"],
            &["haarness", "open chrome-extension://x"],
            &["haarness", "run", "chrome-extension://x/"],
            &["haarness", "browser", "native-host", "--help"],
            &["haarness", "browser", "native-host", "-h"],
        ] {
            assert!(!native_host_requested(&args(other)), "{other:?}");
        }
    }

    #[test]
    fn the_cli_parses_the_browser_commands() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: super::BrowserCommand,
        }
        let cli = Cli::try_parse_from([
            "browser",
            "setup",
            "--browser",
            "chrome-beta",
            "--browser",
            "edge",
            "--manifest-dir",
            "/tmp/a",
            "--json",
            "--quiet",
            "--if-missing",
            "--use-isolated-data-for-chrome",
        ])
        .unwrap();
        let super::BrowserCommand::Setup(setup) = cli.command else {
            panic!("setup")
        };
        assert_eq!(
            setup.browsers,
            [super::BrowserChoice::ChromeBeta, super::BrowserChoice::Edge]
        );
        assert!(
            setup.json && setup.quiet && setup.if_missing && setup.use_isolated_data_for_chrome
        );
        let cli = Cli::try_parse_from([
            "browser",
            "native-host",
            "chrome-extension://accdjhmcgljbadpeanajllmcembhkomn/",
            "--parent-window=12",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            super::BrowserCommand::NativeHost { parent_window: Some(ref window), .. } if window == "12"
        ));
    }
}
