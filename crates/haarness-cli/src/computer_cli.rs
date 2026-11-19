//! `haarness mac`: computer use from the command line, and the `computer-use` view the terminal UI's `/mac` shows.
//! The settings belong to this Mac and apply to every project; only a person gives agents more reach, so the commands
//! that widen them refuse to run inside a program Haarness started. Switching off and tightening always work.

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use haarness_computer::{Computer, policy::Rule};
use haarness_protocol::{ManagementEntry as Entry, ManagementView as View};
use serde_json::{Value, json};

#[derive(Debug, Clone, Args)]
pub struct MacArgs {
    #[command(subcommand)]
    command: Option<MacCommand>,
}

#[derive(Debug, Clone, Subcommand)]
enum MacCommand {
    /// Show whether agents may use this Mac, what macOS allows, and the app rules.
    Status {
        /// Print the status as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Let agents see and use this Mac.
    On,
    /// Stop agents from using this Mac.
    Off,
    /// Show macOS's permission prompts and open System Settings where you allow them.
    Permissions,
    /// Let agents act in an app without asking (a bundle id such as com.apple.TextEdit, or com.jetbrains.*).
    Allow {
        /// The app's bundle id, such as com.apple.TextEdit, or a family such as com.jetbrains.*.
        app: String,
    },
    /// Make agents ask every time before acting in an app.
    Ask {
        /// The app's bundle id, such as com.apple.TextEdit, or a family such as com.jetbrains.*.
        app: String,
    },
    /// Never let agents see, act in or open an app.
    Deny {
        /// The app's bundle id, such as com.apple.TextEdit, or a family such as com.jetbrains.*.
        app: String,
    },
    /// Remove an app's rule.
    Forget {
        /// The app's bundle id, such as com.apple.TextEdit, or a family such as com.jetbrains.*.
        app: String,
    },
}

/// Whether this command runs inside a program Haarness started: an agent's shell command (which names its chat), or
/// anything below a Haarness program (this one's own file counts, whatever it is called) or in a process group Haarness
/// started for a command, which catches a command that cleared that variable and one put in the background.
fn inside_haarness() -> bool {
    haarness_core::process_tree::this_process_inside()
}

/// The management request a command amounts to, for [`Computer::widens`].
fn request(command: &MacCommand) -> Value {
    let app = |app: &str, rule: &str| json!({"action": "app", "bundle": app, "rule": rule});
    match command {
        MacCommand::On => json!({"action": "enable", "enabled": true}),
        MacCommand::Allow { app: name } => app(name, "allow"),
        MacCommand::Ask { app: name } => app(name, "ask"),
        MacCommand::Deny { app: name } => app(name, "deny"),
        MacCommand::Forget { app: name } => app(name, "clear"),
        _ => json!({"action": "get"}),
    }
}

pub async fn run(computer: &Computer, args: &MacArgs) -> Result<()> {
    let command = args
        .command
        .clone()
        .unwrap_or(MacCommand::Status { json: false });
    if let MacCommand::Status { json } = command {
        let status = computer.status().await;
        if json {
            println!("{}", serde_json::to_string_pretty(&status)?);
        } else {
            println!("{}", status_lines(&status).join("\n"));
        }
        return Ok(());
    }
    if computer.widens(&request(&command)) && inside_haarness() {
        bail!(haarness_computer::INSIDE_REFUSAL);
    }
    if !computer.supported() {
        bail!("Computer use needs macOS and a build of Haarness that includes it.");
    }
    match command {
        MacCommand::On => {
            computer.enable(true).map_err(anyhow::Error::msg)?;
            let status = computer.status().await;
            println!("{}", status_lines(&status).join("\n"));
            let allowed = |key: &str| status["permissions"][key].as_bool() == Some(true);
            if !allowed("accessibility") || !allowed("screenRecording") {
                println!(
                    "\nNext, grant Accessibility and Screen Recording in macOS: haarness mac permissions walks you there."
                );
            }
        }
        MacCommand::Off => {
            computer.enable(false).map_err(anyhow::Error::msg)?;
            println!("Computer use is off. Agents no longer see or use this Mac.");
        }
        MacCommand::Permissions => {
            computer
                .request_permissions()
                .await
                .map_err(anyhow::Error::msg)?;
            let status = computer.status().await;
            println!("{}", status_lines(&status).join("\n"));
        }
        MacCommand::Allow { app } => set(computer, &app, Some(Rule::Allow))?,
        MacCommand::Ask { app } => set(computer, &app, Some(Rule::Ask))?,
        MacCommand::Deny { app } => set(computer, &app, Some(Rule::Deny))?,
        MacCommand::Forget { app } => set(computer, &app, None)?,
        MacCommand::Status { .. } => unreachable!("handled above"),
    }
    Ok(())
}

fn set(computer: &Computer, app: &str, rule: Option<Rule>) -> Result<()> {
    computer.set_app(app, rule).map_err(anyhow::Error::msg)?;
    println!(
        "{}",
        match rule {
            Some(Rule::Allow) => format!("{app}: agents go ahead without asking."),
            Some(Rule::Ask) => format!("Agents ask every time before they act in {app}."),
            Some(Rule::Deny) => format!("Agents never see, act in or open {app}."),
            None => format!("{app} has no rule now."),
        }
    );
    Ok(())
}

fn host_name(status: &Value) -> String {
    status["host"]["name"]
        .as_str()
        .or_else(|| status["host"]["bundle"].as_str())
        .map_or_else(|| "the app Haarness runs in".to_owned(), str::to_owned)
}

/// The status in plain lines, for `haarness mac status` and the terminal UI's view.
pub fn status_lines(status: &Value) -> Vec<String> {
    if status["supported"].as_bool() != Some(true) {
        return vec!["Computer use needs macOS and a build of Haarness that includes it.".into()];
    }
    let mut lines = vec![format!(
        "Computer use: {}",
        if status["enabled"].as_bool() == Some(true) {
            "on"
        } else {
            "off"
        }
    )];
    if status["console"].as_bool() != Some(true) {
        lines.push("This Mac's screen belongs to another user, or nobody is signed in, so agents can't use it from this account.".into());
    }
    if let Some(permissions) = status["permissions"].as_object() {
        let host = host_name(status);
        lines.push(format!(
            "Accessibility for {host}: {}",
            allowed(permissions.get("accessibility"))
        ));
        lines.push(format!(
            "Screen Recording for {host}: {}",
            allowed(permissions.get("screenRecording"))
        ));
    }
    let settings = &status["settings"];
    lines.push(format!(
        "Apps without a rule: {}",
        if settings["defaultAct"] == "allow" {
            "agents act without asking"
        } else {
            "agents ask first"
        }
    ));
    lines.push(format!(
        "Ring where the agent clicks: {}",
        if settings["ring"] == false {
            "hidden"
        } else {
            "shown"
        }
    ));
    let apps = settings["apps"].as_array().cloned().unwrap_or_default();
    if apps.is_empty() {
        lines.push("App rules: none (haarness mac allow|ask|deny <bundle id> adds one)".into());
    } else {
        lines.push("App rules:".into());
        for app in apps {
            lines.push(format!(
                "  {}{}: {}",
                app["bundle"].as_str().unwrap_or_default(),
                app["name"]
                    .as_str()
                    .map(|name| format!(" ({name})"))
                    .unwrap_or_default(),
                app["rule"].as_str().unwrap_or_default()
            ));
        }
    }
    lines
}

fn allowed(value: Option<&Value>) -> &'static str {
    if value.and_then(Value::as_bool) == Some(true) {
        "allowed"
    } else {
        "not allowed yet"
    }
}

/// The view behind the terminal UI's `/mac`; Desktop renders `data` with its own controls.
pub fn view(status: Value) -> View {
    let mut lines = Vec::new();
    let mut entries = Vec::new();
    if status["supported"].as_bool() == Some(true) {
        lines.extend(status_lines(&status));
        let on = status["enabled"].as_bool() == Some(true);
        entries.push(Entry {
            label: if on { "Turn off" } else { "Turn on" }.into(),
            description: if on {
                "Agents stop at their next step"
            } else {
                "Agents get the mac_* tools from their next turn"
            }
            .into(),
            request: json!({"action": "enable", "enabled": !on}),
            confirm: None,
            icon: None,
        });
        let granted = |key: &str| status["permissions"][key].as_bool() == Some(true);
        if !granted("accessibility") || !granted("screenRecording") {
            entries.push(Entry {
                label: "Allow in System Settings".into(),
                description: format!(
                    "macOS asks for {}; switch it on in the list that opens",
                    host_name(&status)
                ),
                request: json!({"action": "permissions"}),
                confirm: None,
                icon: None,
            });
        }
        let allow_all = status["settings"]["defaultAct"] == "allow";
        entries.push(Entry {
            label: if allow_all { "Ask before acting in apps without a rule" } else { "Act in apps without a rule without asking" }.into(),
            description: "Apps set to ask every time still ask".into(),
            request: json!({"action": "configure", "defaultAct": if allow_all { "ask" } else { "allow" }}),
            confirm: None,
            icon: None,
        });
        let ring = status["settings"]["ring"] != false;
        entries.push(Entry {
            label: if ring {
                "Hide the ring where the agent clicks"
            } else {
                "Highlight each spot before the agent clicks it"
            }
            .into(),
            description: String::new(),
            request: json!({"action": "configure", "ring": !ring}),
            confirm: None,
            icon: None,
        });
        for app in status["settings"]["apps"].as_array().into_iter().flatten() {
            let bundle = app["bundle"].as_str().unwrap_or_default();
            entries.push(Entry {
                label: format!(
                    "Remove the rule for {}",
                    app["name"].as_str().unwrap_or(bundle)
                ),
                description: format!("{bundle}: {}", app["rule"].as_str().unwrap_or_default()),
                request: json!({"action": "app", "bundle": bundle, "rule": "clear"}),
                confirm: None,
                icon: None,
            });
        }
    } else {
        lines.push("Computer use gives agents this Mac's screen, mouse and keyboard. It is available on macOS, in Haarness builds that ship the helper.".into());
    }
    View {
        title: "Computer use".into(),
        lines,
        entries,
        data: Some(status),
        ..View::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(enabled: bool) -> Value {
        json!({
            "supported": true,
            "console": true,
            "enabled": enabled,
            "terms": {"version": "2", "accepted": true, "acceptedAt": null, "changed": false},
            "permissions": {"accessibility": true, "screenRecording": false},
            "host": {"bundle": "com.apple.Terminal", "name": "Terminal"},
            "settings": {"defaultAct": "ask", "ring": true, "apps": [{"bundle": "com.apple.TextEdit", "name": "TextEdit", "rule": "allow"}]},
        })
    }

    #[test]
    fn off_the_view_turns_on_in_one_step_with_nothing_to_agree_to() {
        let view = view(status(false));
        assert_eq!(view.lines[0], "Computer use: off");
        assert!(
            !view
                .lines
                .iter()
                .any(|line| line.to_lowercase().contains("terms")),
            "{:?}",
            view.lines
        );
        assert_eq!(view.entries[0].label, "Turn on");
        assert_eq!(
            view.entries[0].request,
            json!({"action": "enable", "enabled": true})
        );
        assert!(view.entries[0].confirm.is_none());
        assert!(view.entries.iter().all(|entry| {
            entry.confirm.is_none()
                && !["accept", "withdraw"]
                    .contains(&entry.request["action"].as_str().unwrap_or_default())
        }));
    }

    #[test]
    fn on_the_view_names_the_app_macos_asks_about_and_turns_off_in_one_step() {
        let view = view(status(true));
        assert!(
            view.lines
                .iter()
                .any(|line| line == "Screen Recording for Terminal: not allowed yet")
        );
        assert!(
            view.entries
                .iter()
                .any(|entry| entry.request["action"] == "permissions")
        );
        assert!(view.entries.iter().any(|entry| entry.request
            == json!({"action": "app", "bundle": "com.apple.TextEdit", "rule": "clear"})));
        assert_eq!(view.entries[0].label, "Turn off");
        assert_eq!(
            view.entries[0].request,
            json!({"action": "enable", "enabled": false})
        );
        assert!(view.entries[0].confirm.is_none());
    }

    #[test]
    fn mac_has_no_terms_or_withdraw_command() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            mac: MacArgs,
        }
        for gone in ["terms", "withdraw"] {
            assert!(Cli::try_parse_from(["mac", gone]).is_err(), "{gone}");
        }
        for kept in ["on", "off", "status", "permissions"] {
            assert!(Cli::try_parse_from(["mac", kept]).is_ok(), "{kept}");
        }
    }
}
