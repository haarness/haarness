//! Named application shortcuts. Dialogs, approvals and terminal panes own their
//! keys before these bindings run; configuration cannot steal safety replies.
use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use haarness_protocol::{ManagementField, ManagementView};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) const ACTIONS: &[(&str, &str, &str)] = &[
    ("commands", "Command palette", "ctrl+p"),
    ("sessions", "Sessions", "ctrl+l"),
    ("messages", "Message browser", "alt+m"),
    ("models", "Models", "F2"),
    ("agents", "Agents", "alt+a"),
    ("reasoning", "Reasoning effort", "alt+r"),
    ("usage", "Session usage", "alt+u"),
    ("stash", "Prompt stash", "alt+s"),
    ("stash_save", "Save draft to stash", "ctrl+s"),
    ("editor", "External editor", "alt+e"),
    ("new", "New session", "alt+n"),
    // Upstream's message keys. Alt with an arrow reaches the TUI in every
    // terminal, macOS Terminal's ESC-prefixed form included (`input::meta`),
    // and Alt+Left/Right stay word motions.
    ("messages_previous", "Previous message", "alt+up"),
    ("messages_next", "Next message", "alt+down"),
    ("messages_previous_user", "Previous prompt", "alt+shift+up"),
    ("messages_next_user", "Next prompt", "alt+shift+down"),
    ("messages_first", "First message", "alt+home"),
    ("messages_last_user", "Last prompt", "alt+end"),
];
/// Keys named by their name rather than the character they type.
const NAMED: &[(&str, KeyCode)] = &[
    ("up", KeyCode::Up),
    ("down", KeyCode::Down),
    ("left", KeyCode::Left),
    ("right", KeyCode::Right),
    ("home", KeyCode::Home),
    ("end", KeyCode::End),
    ("pageup", KeyCode::PageUp),
    ("pagedown", KeyCode::PageDown),
];
pub(crate) fn parse(value: &str) -> Result<Option<(KeyCode, KeyModifiers)>> {
    if value == "none" || value.is_empty() {
        return Ok(None);
    }
    let lower = value.to_ascii_lowercase();
    let mut parts = lower.split('+').collect::<Vec<_>>();
    let name = parts.pop().context("Missing key")?;
    let mut modifiers = KeyModifiers::NONE;
    for part in parts {
        modifiers |= match part {
            "ctrl" => KeyModifiers::CONTROL,
            "alt" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => anyhow::bail!("Use ctrl, alt or shift modifiers"),
        };
    }
    let code = if let Some(number) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        ensure!((1..=12).contains(&number), "Use F1 through F12");
        KeyCode::F(number)
    } else if let Some((_, code)) = NAMED.iter().find(|(named, _)| *named == name) {
        // Bare, these move the prompt's cursor and the transcript.
        ensure!(!modifiers.is_empty(), "Add ctrl, alt or shift to {name}");
        *code
    } else {
        ensure!(
            name.chars().count() == 1
                && modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT),
            "Use a modified character, a modified arrow, home, end, pageup or pagedown, or F1–F12"
        );
        KeyCode::Char(name.chars().next().unwrap())
    };
    ensure!(
        !matches!(
            (code, modifiers),
            (KeyCode::Char('c' | 'z'), KeyModifiers::CONTROL)
        ),
        "Ctrl+C and Ctrl+Z are reserved"
    );
    ensure!(
        !(code == KeyCode::Char('m') && modifiers == KeyModifiers::CONTROL),
        "Ctrl+M is Enter in terminals; use F2 or another shortcut"
    );
    Ok(Some((code, modifiers)))
}
pub(crate) fn validate(bindings: &BTreeMap<String, String>) -> Result<()> {
    let mut keys = Vec::new();
    for (action, value) in bindings {
        ensure!(
            ACTIONS.iter().any(|(id, _, _)| id == action),
            "Unknown shortcut action {action}"
        );
        if let Some(key) = parse(value)? {
            ensure!(!keys.contains(&key), "Two actions use {value}");
            keys.push(key);
        }
    }
    Ok(())
}
pub(crate) fn load() -> Result<BTreeMap<String, String>> {
    let mut bindings = ACTIONS
        .iter()
        .map(|(id, _, key)| ((*id).into(), (*key).into()))
        .collect::<BTreeMap<_, _>>();
    if let Some(config) = crate::config::client_config_text() {
        let file: toml::Value = toml::from_str(&config)?;
        if let Some(table) = file
            .get("tui")
            .and_then(|v| v.get("keybinds"))
            .and_then(toml::Value::as_table)
        {
            for (name, value) in table {
                bindings.insert(
                    name.clone(),
                    value
                        .as_str()
                        .context("Keybindings must be strings")?
                        .to_owned(),
                );
            }
        }
    }
    bindings.extend(crate::config::load_state().keybindings);
    migrate_model_shortcut(&mut bindings);
    validate(&bindings)?;
    Ok(bindings)
}
fn migrate_model_shortcut(bindings: &mut BTreeMap<String, String>) {
    if bindings
        .get("models")
        .is_some_and(|key| key.eq_ignore_ascii_case("ctrl+m"))
    {
        // Previous versions saved the full default map. Migrate that unusable
        // binding without taking a function key already assigned by the user.
        let replacement = (2..=12)
            .map(|n| format!("F{n}"))
            .find(|key| {
                !bindings
                    .values()
                    .any(|value| value.eq_ignore_ascii_case(key))
            })
            .unwrap_or_else(|| "none".into());
        bindings.insert("models".into(), replacement);
    }
}
pub(crate) fn resolve(bindings: &BTreeMap<String, String>, key: KeyEvent) -> Option<&str> {
    bindings
        .iter()
        .find(|(_, value)| parse(value).ok().flatten() == Some((key.code, key.modifiers)))
        .map(|(name, _)| name.as_str())
}
pub(crate) fn overrides_default(bindings: &BTreeMap<String, String>, key: KeyEvent) -> bool {
    ACTIONS.iter().any(|(name, _, default)| {
        parse(default).ok().flatten() == Some((key.code, key.modifiers))
            && bindings.get(*name).is_some_and(|value| value != default)
    })
}
pub(crate) fn view(input: &Value) -> Result<ManagementView> {
    let mut bindings = load()?;
    if input["action"] == "save" {
        for (name, _, _) in ACTIONS {
            bindings.insert(
                (*name).into(),
                input[*name].as_str().context("Missing shortcut")?.into(),
            );
        }
        validate(&bindings)?;
        let saved = bindings.clone();
        crate::config::save_state(|s| s.keybindings = saved);
    }
    Ok(ManagementView{title:"Keyboard shortcuts".into(),lines:vec!["Use ctrl+k, alt+r, alt+up, F6 or none. Restart to reload [tui.keybinds] from config.toml; edits here apply immediately.".into()],fields:ACTIONS.iter().map(|(name,label,_)|ManagementField{key:(*name).into(),label:(*label).into(),value:bindings.get(*name).cloned().unwrap_or_default(),required:true,secret:false,choices:vec![]}).collect(),submit:Some(json!({"action":"save"})),..ManagementView::default()})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_default_is_distinct_from_enter_and_old_default_migrates_without_collisions() {
        let defaults = ACTIONS
            .iter()
            .map(|(action, _, key)| (action.to_string(), key.to_string()))
            .collect();
        assert_eq!(
            resolve(&defaults, KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)),
            Some("models")
        );
        assert_eq!(
            resolve(&defaults, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            None
        );
        assert!(parse("ctrl+m").is_err());
        let mut legacy = BTreeMap::from([
            ("models".into(), "ctrl+m".into()),
            ("sessions".into(), "F2".into()),
        ]);
        migrate_model_shortcut(&mut legacy);
        assert_eq!(legacy["models"], "F3");
        assert_eq!(legacy["sessions"], "F2");
        validate(&legacy).unwrap();
    }

    #[test]
    fn message_keys_are_alt_arrows_bound_once_and_named_keys_need_a_modifier() {
        let defaults: BTreeMap<String, String> = ACTIONS
            .iter()
            .map(|(action, _, key)| (action.to_string(), key.to_string()))
            .collect();
        validate(&defaults).unwrap();
        let alt = KeyModifiers::ALT;
        let shift = KeyModifiers::ALT | KeyModifiers::SHIFT;
        for (code, modifiers, action) in [
            (KeyCode::Up, alt, "messages_previous"),
            (KeyCode::Down, alt, "messages_next"),
            (KeyCode::Up, shift, "messages_previous_user"),
            (KeyCode::Down, shift, "messages_next_user"),
            (KeyCode::Home, alt, "messages_first"),
            (KeyCode::End, alt, "messages_last_user"),
        ] {
            assert_eq!(
                resolve(&defaults, KeyEvent::new(code, modifiers)),
                Some(action)
            );
        }
        // Word motion, the prompt's own keys and plain scrolling stay free.
        for (code, modifiers) in [
            (KeyCode::Left, alt),
            (KeyCode::Right, alt),
            (KeyCode::Char('b'), alt),
            (KeyCode::Char('f'), alt),
            (KeyCode::Up, KeyModifiers::NONE),
            (KeyCode::Home, KeyModifiers::CONTROL),
            (KeyCode::End, KeyModifiers::CONTROL),
            (KeyCode::PageUp, KeyModifiers::NONE),
        ] {
            assert_eq!(resolve(&defaults, KeyEvent::new(code, modifiers)), None);
        }
        assert_eq!(
            parse("Ctrl+PageDown").unwrap(),
            Some((KeyCode::PageDown, KeyModifiers::CONTROL))
        );
        assert!(parse("up").is_err());
        assert!(parse("end").is_err());
        assert!(parse("alt+upward").is_err());
    }
}
