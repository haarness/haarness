//! App policy for computer use. A few groups of apps are locked whatever the settings say (password managers,
//! Haarness and its host app, typing into terminals and script runners); every other app follows the person's app
//! rules and the approvals they give.

use serde::{Deserialize, Serialize};

/// A person's rule for one app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rule {
    /// Agents act there without asking.
    Allow,
    /// Every action raises an approval, and none of the answers carries over.
    Ask,
    /// Off limits: no actions, no launching, and its windows are blanked out of screenshots.
    Deny,
}

impl Rule {
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// Invisible to agents: password managers and the system's sign-in and authorization windows.
pub const SECRETS: &[&str] = &[
    "com.apple.Passwords",
    "com.apple.keychainaccess",
    "com.apple.SecurityAgent",
    "com.apple.LocalAuthentication.UIAgent",
    "com.apple.CoreAuthUI",
    "com.apple.loginwindow",
    "com.apple.ScreenSaver.Engine",
    // Sign-in to a network share, and iCloud Keychain's prompts.
    "com.apple.NetAuthAgent",
    "com.apple.security.Keychain-Circle-Notification",
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "com.agilebits.onepassword-osx",
    "com.bitwarden.desktop",
    "com.lastpass.LastPass",
    "com.dashlane.dashlanephonefinal",
    "com.keepassxc.keepassxc",
    "org.keepassxc.keepassxc",
    "me.proton.pass.electron",
    "com.nordpass.macos.NordPass",
    "com.enpass.Enpass",
];

/// Haarness and the macOS permission prompts. Being able to click Allow on its own approval card, or on a
/// permission prompt, would let an agent sidestep everything else, so these are invisible as well.
pub const HAARNESS: &[&str] = &[
    "dev.haarness.desktop",
    "dev.haarness.desktop.*",
    // Desktop run from a source checkout is the stock Electron app.
    "com.github.Electron",
    "com.apple.UserNotificationCenter",
    "com.apple.TCCPromptAgent",
    // The prompt asking to allow Accessibility, and Screen Recording's prompts and picker.
    "com.apple.accessibility.universalAccessAuthWarn",
    "com.apple.screencaptureui",
];

/// Terminals. Whatever is typed there runs as a command, outside the shell tool's approvals and outside the check that
/// only a person gives agents more reach (`haarness mac allow …` typed into a terminal is not a program Haarness
/// started), so agents click and scroll there but never type. A terminal can also show Haarness's terminal UI.
pub const TERMINALS: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "com.mitchellh.ghostty",
    "dev.warp.Warp-Stable",
    "dev.warp.Warp",
    "io.alacritty",
    "org.alacritty",
    "net.kovidgoyal.kitty",
    "com.github.wez.wezterm",
    "co.zeit.hyper",
    "org.tabby",
    "com.raphaelamorim.rio",
    "dev.commandline.waveterm",
    "com.termius-dmg.mac",
];

/// Script runners, automation editors, REPLs, notebooks and launcher command lines: what is typed there becomes a
/// script, a macro or a command they run, so they are treated as terminals for typing.
pub const SCRIPT_RUNNERS: &[&str] = &[
    "com.apple.ScriptEditor2",
    "com.apple.Automator",
    "com.apple.shortcuts",
    "com.latenightsw.ScriptDebugger8",
    "com.latenightsw.ScriptDebugger",
    "com.stairways.keyboardmaestro.editor",
    "org.hammerspoon.Hammerspoon",
    "com.runningwithcrayons.Alfred",
    "com.raycast.macos",
    "org.python.IDLE",
    "org.jupyter.jupyterlab-desktop",
];

/// Editors whose ordinary typing runs shell commands (`:!` in Vim, `M-!` in Emacs) and which open terminals of their
/// own: treated as script runners for typing, and as terminals while an approval waits.
pub const SHELL_EDITORS: &[&str] = &[
    "org.vim.MacVim",
    "com.qvacua.VimR",
    "com.neovide.neovide",
    "org.gnu.Emacs",
];

/// Code editors. Agents work in them like in any other app, but their built-in terminal, like a terminal app, can show
/// Haarness's terminal UI with an approval card that takes keys and clicks, and their menus and command palette run
/// selected text, the open file or a task as a command, so a person approves them even in Auto.
pub const EDITORS: &[&str] = &[
    "com.microsoft.VSCode",
    "com.microsoft.VSCodeInsiders",
    "com.vscodium",
    "com.todesktop.230313mzl4w4u92",
    "com.exafunction.windsurf",
    "dev.zed.Zed",
    "dev.zed.Zed-Preview",
    "com.apple.dt.Xcode",
    "com.jetbrains.*",
    "com.google.android.studio",
    "com.sublimetext.4",
    "com.sublimetext.3",
    "com.panic.Nova",
    "com.barebones.bbedit",
];

/// System Settings, which agents work in like any other app, apart from its Privacy & Security pane.
pub const SYSTEM_SETTINGS: &[&str] = &["com.apple.systempreferences", "com.apple.Settings"];

/// The Privacy & Security pane of System Settings, as the helper names an open pane: what agents may do on this Mac
/// (Accessibility, Automation, Screen Recording, Full Disk Access) is set there, so it stays look-only.
const PRIVACY_PANES: &[&str] = &[
    "com.apple.settings.PrivacySecurity.extension.*",
    "com.apple.preference.security",
];

/// The Dock: a drop onto an app's icon opens the dropped item in that app.
pub const DOCK: &str = "com.apple.dock";

/// The locked group a bundle falls into, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Secret,
    Haarness,
    /// The app this Haarness server runs in (the terminal app of a terminal session).
    Host,
    /// Terminals and script runners: no typing.
    Runner,
    Open,
}

/// The kind of input an action sends, as the locked groups judge it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// A plain left click.
    Click,
    Scroll,
    /// Moving the pointer only.
    Hover,
    /// Typing, keys, drags and clicks with modifiers or other buttons.
    Input,
    Launch,
    Quit,
}

/// Checks a bundle identifier, or with `pattern` a family such as `com.jetbrains.*`.
#[must_use]
pub fn valid_bundle(text: &str, pattern: bool) -> bool {
    let body = if pattern {
        text.strip_suffix(".*").unwrap_or(text)
    } else {
        text
    };
    !body.is_empty()
        && body.len() <= 255
        && body
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// Whether `bundle` matches `pattern`: the same identifier, or a prefix pattern ending in `.*`.
#[must_use]
pub fn matches(bundle: &str, pattern: &str) -> bool {
    if bundle.is_empty() {
        return false;
    }
    match pattern.strip_suffix(".*") {
        Some(stem) => {
            bundle == stem
                || bundle
                    .strip_prefix(stem)
                    .is_some_and(|rest| rest.starts_with('.'))
        }
        None => bundle == pattern,
    }
}

fn listed(bundle: &str, list: &[&str]) -> bool {
    list.iter().any(|pattern| matches(bundle, pattern))
}

/// The app that launched this server (from `__CFBundleIdentifier`): Haarness Desktop, or the terminal app of a
/// terminal session. Accessibility and Screen Recording are granted to this app.
#[must_use]
pub fn responsible_app() -> Option<String> {
    let bundle = std::env::var("__CFBundleIdentifier").ok()?;
    valid_bundle(&bundle, false).then_some(bundle)
}

/// The launching app when it is something other than Haarness, typically a terminal. It is look-only, because its
/// window shows this very chat and its approval prompts.
#[must_use]
pub fn host_app() -> Option<String> {
    responsible_app().filter(|bundle| !listed(bundle, HAARNESS) && !listed(bundle, SECRETS))
}

#[must_use]
pub fn group(bundle: &str, host: Option<&str>) -> Group {
    if listed(bundle, SECRETS) {
        Group::Secret
    } else if listed(bundle, HAARNESS) {
        Group::Haarness
    } else if host == Some(bundle) {
        Group::Host
    } else if listed(bundle, TERMINALS)
        || listed(bundle, SCRIPT_RUNNERS)
        || listed(bundle, SHELL_EDITORS)
    {
        Group::Runner
    } else {
        Group::Open
    }
}

/// Whether `bundle` may be showing Haarness's terminal UI, and with it an approval card that a click or a key would
/// answer: a terminal, or a code editor with its built-in terminal.
#[must_use]
pub fn may_show_cards(bundle: &str) -> bool {
    listed(bundle, TERMINALS) || listed(bundle, SHELL_EDITORS) || listed(bundle, EDITORS)
}

/// The refusal for an action in System Settings' Privacy & Security pane (`pane`, as the helper names the open pane),
/// where only looking and scrolling are allowed. A helper that cannot tell the pane names none, and System Settings
/// then follows the app rules and approvals as a whole.
#[must_use]
pub fn pane_refusal(bundle: &str, pane: Option<&str>, gesture: Gesture) -> Option<String> {
    // The pane's own process can own its elements: then the target's bundle is the pane.
    let private = (listed(bundle, PRIVACY_PANES)
        || (listed(bundle, SYSTEM_SETTINGS)
            && pane.is_some_and(|pane| listed(pane, PRIVACY_PANES))))
        && !matches!(gesture, Gesture::Scroll | Gesture::Hover);
    private.then(|| {
        "Privacy & Security in System Settings decides what apps and agents may do on this Mac, so agents only look there. Tell the person what to change.".to_owned()
    })
}

/// Whether Auto's reviewer may answer for actions in `bundle`, given the System Settings pane the helper named. Not in
/// terminals, script runners and editors that run shell commands, where a click on Paste or Run runs code it never
/// sees; not in code editors, whose menus and command palette run selected text, the open file or a task; and not in
/// System Settings when the helper could not tell which pane is open, which may be Privacy & Security.
#[must_use]
pub fn reviewable(bundle: &str, pane: Option<&str>) -> bool {
    group(bundle, None) != Group::Runner
        && !listed(bundle, EDITORS)
        && !listed(bundle, PRIVACY_PANES)
        && (pane.is_some() || !listed(bundle, SYSTEM_SETTINGS))
}

/// The refusal for a click on Paste in a terminal or script runner (the helper reports the target as a Paste menu
/// item): what it pastes runs there as a command.
#[must_use]
pub fn paste_refusal(name: &str) -> String {
    format!(
        "{name} runs whatever is pasted into it, outside Haarness's approvals, so agents never paste there. Run commands with the shell tool instead."
    )
}

/// The refusal for keys or typing aimed at a terminal inside another app (an editor's terminal panel), which runs
/// what is typed just like a terminal app does.
#[must_use]
pub fn terminal_refusal(name: &str) -> String {
    format!(
        "That is a terminal inside {name}: what is typed there runs as a command outside Haarness's approvals, so agents never type or press keys in it. Run commands with the shell tool instead."
    )
}

/// The refusal for a locked group, written as advice for the agent, or `None` when the app rules decide.
#[must_use]
pub fn refusal(bundle: &str, name: &str, gesture: Gesture, host: Option<&str>) -> Option<String> {
    match group(bundle, host) {
        Group::Secret => Some(format!(
            "{name} stores passwords or handles sign-in, so computer use stays out of it. Hand this step to the person."
        )),
        Group::Haarness => Some(format!(
            "{name} belongs to Haarness or to a macOS permission prompt. Agents are kept out of both; the person has to do this step."
        )),
        _ if gesture == Gesture::Launch => None,
        Group::Host => Some(format!(
            "{name} shows this very chat and its approval prompts, so agents only look at it. The person has to do this step."
        )),
        Group::Runner if gesture == Gesture::Input => Some(format!(
            "{name} runs whatever is typed into it, outside Haarness's approvals, so agents may click and scroll there but never type or press keys. Run commands with the shell tool instead, under its usual approvals; for options it stores as text, `defaults read {bundle}` lists many."
        )),
        _ => None,
    }
}

/// Whether agents may read `bundle` at all.
#[must_use]
pub fn readable(bundle: &str, rule: Option<Rule>) -> bool {
    !listed(bundle, SECRETS) && !listed(bundle, HAARNESS) && rule != Some(Rule::Deny)
}

/// Everything blanked out of screenshots, window lists and outlines: the invisible groups plus apps set to Never.
#[must_use]
pub fn hidden<'a>(denied: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    SECRETS
        .iter()
        .chain(HAARNESS)
        .map(|pattern| (*pattern).to_owned())
        .chain(denied.into_iter().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_groups_refuse_what_no_rule_opens() {
        let host = Some("com.googlecode.iterm2");
        assert!(refusal("com.1password.1password", "1Password", Gesture::Click, None).is_some());
        assert!(
            refusal(
                "com.1password.1password",
                "1Password",
                Gesture::Launch,
                None
            )
            .is_some()
        );
        assert!(refusal("dev.haarness.desktop", "Haarness", Gesture::Scroll, None).is_some());
        assert!(
            refusal(
                "dev.haarness.desktop.helper",
                "Haarness",
                Gesture::Click,
                None
            )
            .is_some()
        );
        assert!(refusal("com.googlecode.iterm2", "iTerm", Gesture::Click, host).is_some());
        assert!(refusal("com.googlecode.iterm2", "iTerm", Gesture::Quit, host).is_some());
        assert!(refusal("com.googlecode.iterm2", "iTerm", Gesture::Launch, host).is_none());
        // Typing into a terminal or a script runner runs it: refused. Everything else there is the app rules' to say.
        assert!(refusal("com.apple.Terminal", "Terminal", Gesture::Click, None).is_none());
        assert!(refusal("com.apple.Terminal", "Terminal", Gesture::Quit, None).is_none());
        assert!(refusal("com.apple.Terminal", "Terminal", Gesture::Input, None).is_some());
        assert!(
            refusal(
                "com.apple.ScriptEditor2",
                "Script Editor",
                Gesture::Input,
                None
            )
            .is_some()
        );
        // Browsers, System Settings and code editors have no fixed lock any more.
        for (bundle, name) in [
            ("com.apple.Safari", "Safari"),
            ("com.google.Chrome", "Chrome"),
            ("com.apple.systempreferences", "System Settings"),
            ("com.jetbrains.intellij", "IntelliJ IDEA"),
            ("com.microsoft.VSCode", "Visual Studio Code"),
        ] {
            for gesture in [Gesture::Click, Gesture::Input, Gesture::Quit] {
                assert!(refusal(bundle, name, gesture, None).is_none(), "{name}");
            }
        }
        assert!(refusal("com.apple.TextEdit", "TextEdit", Gesture::Input, None).is_none());
    }

    #[test]
    fn editors_that_run_shell_commands_take_no_typing_and_privacy_settings_stay_look_only() {
        for (bundle, name) in [("org.vim.MacVim", "MacVim"), ("org.gnu.Emacs", "Emacs")] {
            assert!(
                refusal(bundle, name, Gesture::Input, None).is_some(),
                "{name}"
            );
            assert!(
                refusal(bundle, name, Gesture::Click, None).is_none(),
                "{name}"
            );
            assert!(may_show_cards(bundle), "{name}");
        }
        assert!(refusal("dev.commandline.waveterm", "Wave", Gesture::Input, None).is_some());
        assert!(refusal("com.neovide.neovide", "Neovide", Gesture::Input, None).is_some());
        assert!(refusal("org.python.IDLE", "IDLE", Gesture::Input, None).is_some());
        assert!(refusal("com.raycast.macos", "Raycast", Gesture::Input, None).is_some());
        let privacy = Some("com.apple.settings.PrivacySecurity.extension");
        let settings = "com.apple.systempreferences";
        assert!(pane_refusal(settings, privacy, Gesture::Click).is_some());
        assert!(pane_refusal(settings, privacy, Gesture::Input).is_some());
        assert!(pane_refusal(settings, privacy, Gesture::Scroll).is_none());
        assert!(
            pane_refusal(
                settings,
                Some("com.apple.Displays-Settings.extension"),
                Gesture::Click
            )
            .is_none()
        );
        // A helper that cannot tell the pane: System Settings follows the app rules, with a person to answer.
        assert!(pane_refusal(settings, None, Gesture::Click).is_none());
        // The pane's own process owning the element.
        let extension = "com.apple.settings.PrivacySecurity.extension";
        assert!(pane_refusal(extension, None, Gesture::Click).is_some());
        assert!(pane_refusal(extension, None, Gesture::Scroll).is_none());
        assert!(pane_refusal("com.apple.TextEdit", privacy, Gesture::Click).is_none());
    }

    #[test]
    fn auto_s_reviewer_leaves_terminals_editors_and_unnamed_settings_panes_to_a_person() {
        assert!(reviewable("com.apple.TextEdit", None));
        assert!(reviewable("com.apple.Safari", None));
        for bundle in [
            "com.apple.Terminal",
            "com.apple.ScriptEditor2",
            "org.gnu.Emacs",
            "com.microsoft.VSCode",
            "com.todesktop.230313mzl4w4u92",
            "com.jetbrains.pycharm",
            "com.apple.dt.Xcode",
            "com.sublimetext.4",
            "com.apple.settings.PrivacySecurity.extension",
        ] {
            assert!(!reviewable(bundle, None), "{bundle}");
        }
        let settings = "com.apple.systempreferences";
        assert!(!reviewable(settings, None));
        assert!(reviewable(
            settings,
            Some("com.apple.Displays-Settings.extension")
        ));
    }

    #[test]
    fn terminals_and_editors_may_show_an_approval_card() {
        assert!(may_show_cards("com.googlecode.iterm2"));
        assert!(may_show_cards("com.jetbrains.intellij"));
        assert!(may_show_cards("com.microsoft.VSCode"));
        assert!(!may_show_cards("com.apple.TextEdit"));
        assert!(!may_show_cards("com.apple.ScriptEditor2"));
        assert!(!may_show_cards("com.apple.Safari"));
    }

    #[test]
    fn bundle_patterns_match_whole_parts() {
        assert!(matches("com.jetbrains.intellij", "com.jetbrains.*"));
        assert!(matches("com.jetbrains", "com.jetbrains.*"));
        assert!(!matches("com.jetbrainsx.app", "com.jetbrains.*"));
        assert!(!matches("", "*"));
        assert!(valid_bundle("com.apple.Notes", false));
        assert!(valid_bundle("com.jetbrains.*", true));
        assert!(!valid_bundle("com.jetbrains.*", false));
        assert!(!valid_bundle("../etc", false));
        assert!(!valid_bundle("", false));
    }

    #[test]
    fn denied_apps_are_hidden_and_unreadable() {
        let hidden = hidden(["com.apple.MobileSMS"]);
        assert!(hidden.iter().any(|item| item == "com.apple.MobileSMS"));
        assert!(hidden.iter().any(|item| item == "dev.haarness.desktop"));
        assert!(!readable("com.apple.MobileSMS", Some(Rule::Deny)));
        assert!(!readable("com.apple.keychainaccess", None));
        assert!(readable("com.apple.Notes", Some(Rule::Allow)));
    }
}
