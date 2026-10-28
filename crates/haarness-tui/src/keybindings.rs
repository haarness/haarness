//! Named application shortcuts. Dialogs, approvals and terminal panes own their
//! keys before these bindings run; configuration cannot steal safety replies.
use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use haarness_protocol::{ManagementField, ManagementView};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Opens or closes the fold nearest the prompt: a tool's output, a long
/// prompt, a thought.
pub(crate) const FOLD_ACTION: &str = "fold";

pub(crate) const ACTIONS: &[(&str, &str, &str)] = &[
    ("commands", "Command palette", "ctrl+p"),
    (FOLD_ACTION, "Open or close a fold", "ctrl+r"),
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
    // Message keys. Alt with an arrow reaches the TUI in every
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
fn defaults() -> BTreeMap<String, String> {
    ACTIONS
        .iter()
        .map(|(id, _, key)| ((*id).into(), (*key).into()))
        .collect()
}

/// The `[tui.keybinds]` lines of config.toml. A value that is not a string
/// is kept as `None`, so it is reported rather than silently dropped.
fn configured() -> BTreeMap<String, Option<String>> {
    crate::config::client_config_text()
        .and_then(|config| toml::from_str::<toml::Value>(&config).ok())
        .and_then(|file| file.get("tui")?.get("keybinds")?.as_table().cloned())
        .map(|table| {
            table
                .into_iter()
                .map(|(name, value)| (name, value.as_str().map(str::to_owned)))
                .collect()
        })
        .unwrap_or_default()
}

/// The shortcuts in force, and the labels of the settings worth saying did
/// not apply (see [`Settled::unapplied`]). Each is left out on its own and
/// the rest apply; a shortcut the person chose wins over a default.
pub(crate) fn load() -> (BTreeMap<String, String>, Vec<String>) {
    let settled = settle(configured(), crate::config::load_state().keybindings);
    let labels = settled
        .unapplied
        .iter()
        .map(|name| {
            ACTIONS
                .iter()
                .find(|(id, _, _)| id == name)
                .map_or_else(|| name.clone(), |(_, label, _)| (*label).to_owned())
        })
        .collect();
    (settled.bindings, labels)
}

struct Settled {
    bindings: BTreeMap<String, String>,
    /// Settings that did not apply and are worth saying, by action: an
    /// unknown action or a key that cannot be bound in config.toml, or a key
    /// two of the person's own shortcuts share. Never a default the person's
    /// own key displaced (they chose that key), nor what tui-state.toml holds
    /// that this version cannot apply (a newer Haarness wrote it; a save
    /// keeps it).
    unapplied: Vec<String>,
}

/// The shortcuts from the defaults, config.toml's `[tui.keybinds]` and
/// tui-state.toml (which /keybinds writes and which wins per action).
fn settle(config: BTreeMap<String, Option<String>>, state: BTreeMap<String, String>) -> Settled {
    let mut chosen = config
        .into_iter()
        .map(|(name, value)| (name, (value, false)))
        .collect::<BTreeMap<_, _>>();
    chosen.extend(
        state
            .into_iter()
            .map(|(name, value)| (name, (Some(value), true))),
    );
    if chosen
        .get("models")
        .and_then(|(value, _)| value.as_deref())
        .is_some_and(|key| key.eq_ignore_ascii_case("ctrl+m"))
    {
        // Previous versions saved ctrl+m, which is Enter in terminals.
        let mut merged = defaults();
        merged.extend(
            chosen
                .iter()
                .filter_map(|(name, (value, _))| Some((name.clone(), value.clone()?))),
        );
        migrate_model_shortcut(&mut merged);
        if let Some((value, _)) = chosen.get_mut("models") {
            *value = merged.remove("models");
        }
    }
    let mut bindings = defaults();
    let mut unapplied = Vec::new();
    // The person's own shortcuts first, in name order: each takes its key
    // unless another of theirs already has it.
    let mut taken = Vec::new();
    let mut own = Vec::new();
    for (name, (value, from_state)) in chosen {
        let key = value
            .as_deref()
            .filter(|_| bindings.contains_key(&name))
            .and_then(|value| parse(value).ok());
        match (key, value) {
            (Some(key), Some(value)) if !key.is_some_and(|key| taken.contains(&key)) => {
                taken.extend(key);
                bindings.insert(name.clone(), value);
                own.push(name);
            }
            (Some(_), Some(_)) => unapplied.push(name),
            _ if !from_state => unapplied.push(name),
            _ => {}
        }
    }
    // Then the defaults left, each unless its key went to one of the
    // person's shortcuts: that default is unbound, quietly, rather than the
    // person's choice refused.
    for (name, _, default) in ACTIONS {
        if !own.iter().any(|own| own == name)
            && parse(default)
                .ok()
                .flatten()
                .is_some_and(|key| taken.contains(&key))
        {
            bindings.insert((*name).into(), "none".into());
        }
    }
    Settled {
        bindings,
        unapplied,
    }
}

/// What a /keybinds save keeps in tui-state.toml, from the form's `bindings`
/// and what was `loaded` before it:
/// - the shortcuts that differ from what config.toml and the defaults give,
///   so an older Haarness meets an action it does not know only when that one
///   was changed, and a later default is not pinned to today's;
/// - every action whose config.toml line did not apply, so the value set here
///   settles it and the launch notice goes;
/// - what this version cannot apply from the file (a newer Haarness's action
///   or key), unless that action was edited here.
fn saved_state(
    bindings: &BTreeMap<String, String>,
    loaded: &BTreeMap<String, String>,
    config: BTreeMap<String, Option<String>>,
    state: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let baseline = settle(config, BTreeMap::new());
    let mut saved = bindings
        .iter()
        .filter(|(name, value)| {
            baseline.bindings.get(*name) != Some(*value) || baseline.unapplied.contains(name)
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    for (name, value) in state {
        let applies = ACTIONS.iter().any(|(id, _, _)| *id == name) && parse(&value).is_ok();
        if !applies && bindings.get(&name) == loaded.get(&name) {
            saved.insert(name, value);
        }
    }
    saved
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
/// The chat's main keys that are not shortcuts to rebind, listed above the
/// shortcuts. Editing keys (Ctrl+A/E/W/K/U, Ctrl+- undo) work as in any
/// prompt and are left to the reference.
const FIXED_KEYS: &[&str] = &[
    "Fixed: esc esc stop · ctrl+c clear, then stop · alt+enter queue · ctrl+g approval · tab agent · ctrl+y approvals · ctrl+b sidebar · ctrl+t thinking",
];

/// What the shortcuts form did with a save.
pub(crate) enum Saving {
    /// The form was only shown.
    None,
    Saved,
    /// `tui-state.toml` could not take them: the shortcuts this save changed,
    /// by action, which apply until it can.
    NotSaved(BTreeMap<String, String>),
}

/// `changes`, by action, laid over `bindings`.
fn lay_over(bindings: &mut BTreeMap<String, String>, changes: &BTreeMap<String, String>) {
    for (action, key) in changes {
        bindings.insert(action.clone(), key.clone());
    }
}

/// `unsaved` (shortcuts no save could write, by action) laid over
/// `bindings`, the ones loaded now. One whose key another action holds by
/// now (another TUI took it) is dropped from `unsaved` and left out, as is
/// one that would then clash in its turn: two actions never share a key.
pub(crate) fn lay_over_unsaved(
    bindings: &mut BTreeMap<String, String>,
    unsaved: &mut BTreeMap<String, String>,
) {
    let key = |value: &String| parse(value).ok().flatten();
    loop {
        let mut laid = bindings.clone();
        lay_over(&mut laid, unsaved);
        let clashing = unsaved
            .keys()
            .filter(|action| {
                let held = laid.get(*action).and_then(key);
                held.is_some()
                    && laid
                        .iter()
                        .any(|(other, value)| other != *action && key(value) == held)
            })
            .cloned()
            .collect::<Vec<_>>();
        if clashing.is_empty() {
            *bindings = laid;
            return;
        }
        for action in clashing {
            unsaved.remove(&action);
        }
    }
}

/// The shortcuts form, with `unsaved` laid over what is saved now, and what
/// a save from it did. A save writes only the shortcuts changed in the form
/// since it opened (it carries the values it opened with), with `unsaved`,
/// over what is saved when it writes: another TUI may have changed the
/// others meanwhile. A key another TUI took since is refused, as a key two
/// fields of the form share is.
pub(crate) fn view(
    input: &Value,
    unsaved: &BTreeMap<String, String>,
) -> Result<(ManagementView, Saving)> {
    let (mut bindings, _) = load();
    let mut changes = unsaved.clone();
    lay_over_unsaved(&mut bindings, &mut changes);
    let mut saving = Saving::None;
    if input["action"] == "save" {
        for (name, _, _) in ACTIONS {
            let value = input[*name].as_str().context("Missing shortcut")?;
            if input["opened"][*name].as_str() != Some(value) {
                changes.insert((*name).into(), value.into());
            }
        }
        lay_over(&mut bindings, &changes);
        validate(&bindings)?;
        let mut clash = None;
        let saved = crate::config::save_state(|state| {
            let config = configured();
            let loaded = settle(config.clone(), state.keybindings.clone()).bindings;
            let mut bindings = loaded.clone();
            lay_over(&mut bindings, &changes);
            match validate(&bindings) {
                Ok(()) => {
                    let kept = std::mem::take(&mut state.keybindings);
                    state.keybindings = saved_state(&bindings, &loaded, config, kept);
                }
                // Another TUI took one of these keys since the form opened.
                Err(error) => clash = Some(error),
            }
        });
        if let Some(error) = clash {
            return Err(error);
        }
        saving = if saved {
            Saving::Saved
        } else {
            Saving::NotSaved(changes)
        };
    }
    let view = ManagementView{title:"Keyboard shortcuts".into(),lines:FIXED_KEYS.iter().map(|line| (*line).to_owned()).chain(["Use ctrl+k, alt+r, alt+up, F6 or none. Restart to reload [tui.keybinds] from config.toml; edits here apply immediately.".to_owned()]).collect(),fields:ACTIONS.iter().map(|(name,label,_)|ManagementField{key:(*name).into(),label:(*label).into(),value:bindings.get(*name).cloned().unwrap_or_default(),required:true,secret:false,choices:vec![]}).collect(),submit:Some(json!({"action":"save","opened":bindings})),..ManagementView::default()};
    Ok((view, saving))
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

    fn config(entries: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        entries
            .iter()
            .map(|(name, value)| ((*name).into(), value.map(str::to_owned)))
            .collect()
    }

    fn state(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect()
    }

    #[test]
    fn an_older_full_map_still_loads_and_gains_the_new_defaults() {
        // What 0.1.2 saved from /keybinds: every action it knew, defaults
        // included, with the models key it used to default to.
        let old = ACTIONS[..11]
            .iter()
            .map(|(name, _, key)| (*name, *key))
            .chain([("models", "ctrl+m")])
            .collect::<Vec<_>>();
        let settled = settle(BTreeMap::new(), state(&old));
        assert!(settled.unapplied.is_empty());
        assert_eq!(
            settled.bindings["models"], "F2",
            "the unusable ctrl+m moves to a free function key"
        );
        assert_eq!(settled.bindings["messages_previous"], "alt+up");
        validate(&settled.bindings).unwrap();
    }

    #[test]
    fn each_setting_that_cannot_apply_is_left_out_alone_and_named() {
        let settled = settle(
            config(&[
                ("frobnicate", Some("alt+q")),
                ("usage", Some("F6")),
                ("agents", Some("ctrl+c")),
                ("reasoning", None),
            ]),
            state(&[("stash", "F6")]),
        );
        assert_eq!(
            settled.unapplied,
            ["agents", "frobnicate", "reasoning", "usage"]
        );
        // Two of the person's own on one key: the first by name keeps it, the
        // other keeps its default.
        let bindings = &settled.bindings;
        assert_eq!(bindings["stash"], "F6");
        assert_eq!(bindings["usage"], "alt+u");
        assert_eq!(bindings["agents"], "alt+a");
        assert_eq!(bindings["reasoning"], "alt+r");
        assert!(!bindings.contains_key("frobnicate"));
        validate(bindings).unwrap();
    }

    #[test]
    fn a_shortcut_the_person_chose_wins_over_a_new_default_without_a_word() {
        let settled = settle(config(&[("sessions", Some("alt+up"))]), BTreeMap::new());
        assert_eq!(settled.bindings["sessions"], "alt+up");
        assert_eq!(settled.bindings["messages_previous"], "none");
        assert!(settled.unapplied.is_empty(), "{:?}", settled.unapplied);
        assert_eq!(
            resolve(
                &settled.bindings,
                KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)
            ),
            Some("sessions")
        );
        validate(&settled.bindings).unwrap();
        // Setting Previous message to none in /keybinds changes nothing to
        // save, and nothing is said either way.
        let mut form = settled.bindings.clone();
        form.insert("messages_previous".into(), "none".into());
        let saved = saved_state(
            &form,
            &settled.bindings,
            config(&[("sessions", Some("alt+up"))]),
            BTreeMap::new(),
        );
        assert!(saved.is_empty(), "{saved:?}");
    }

    #[test]
    fn a_save_settles_a_config_line_that_did_not_apply() {
        let lines = config(&[("usage", Some("ctrl+c"))]);
        let settled = settle(lines.clone(), BTreeMap::new());
        assert_eq!(settled.unapplied, ["usage"]);
        // The form shows the default, and saving it as it is settles the line.
        let saved = saved_state(
            &settled.bindings,
            &settled.bindings,
            lines.clone(),
            BTreeMap::new(),
        );
        assert_eq!(saved, state(&[("usage", "alt+u")]));
        assert!(settle(lines, saved).unapplied.is_empty());
    }

    #[test]
    fn a_save_keeps_only_what_differs_and_what_a_newer_haarness_wrote() {
        let lines = config(&[("usage", Some("F6"))]);
        let file = state(&[
            ("future_action", "alt+z"),
            ("messages_next", "alt+hyper+down"),
        ]);
        let loaded = settle(lines.clone(), file.clone());
        // Not said: this version cannot apply them, a newer one wrote them.
        assert!(loaded.unapplied.is_empty(), "{:?}", loaded.unapplied);
        assert_eq!(loaded.bindings["messages_next"], "alt+down");
        let mut form = loaded.bindings.clone();
        form.insert("sessions".into(), "alt+up".into());
        form.insert("messages_previous".into(), "none".into());
        // Put back to its default over config.toml's F6: that is a change too.
        form.insert("usage".into(), "alt+u".into());
        assert_eq!(
            saved_state(&form, &loaded.bindings, lines.clone(), file.clone()),
            state(&[
                ("future_action", "alt+z"),
                ("messages_next", "alt+hyper+down"),
                ("messages_previous", "none"),
                ("sessions", "alt+up"),
                ("usage", "alt+u"),
            ])
        );
        // An edited action takes the edit; the newer action is still kept.
        form.insert("messages_next".into(), "ctrl+n".into());
        let saved = saved_state(&form, &loaded.bindings, lines.clone(), file);
        assert_eq!(saved["messages_next"], "ctrl+n");
        assert_eq!(saved["future_action"], "alt+z");
        // Nothing changed, nothing new written.
        assert!(saved_state(&loaded.bindings, &loaded.bindings, lines, BTreeMap::new()).is_empty());
    }
}
