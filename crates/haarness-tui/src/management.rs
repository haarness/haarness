//! Rendering-independent state for server-authored management forms.
use crate::{
    api::ApiClient,
    state::{App, OptionEntry, OptionsKind, Overlay},
};
use anyhow::{Context, Result};
use haarness_protocol::ManagementView;
use serde_json::{Value, json};

/// How many of a view's lines are shown above its choices before the rest
/// moves to a separate page.
pub(crate) const PREFACE_LINES: usize = 4;

#[derive(Clone, Default)]
pub(crate) struct Management {
    pub resource: String,
    pub view: ManagementView,
    pub field: usize,
    pub pending: Option<Value>,
    pub details: bool,
}
impl App {
    pub(crate) async fn open_management(
        &mut self,
        resource: &str,
        input: &Value,
        client: &ApiClient,
    ) -> Result<()> {
        let mut input = input.clone();
        if resource == "sharing" {
            input["threadId"] = json!(self.thread.id);
        }
        let input = &input;
        let view: ManagementView = if resource == "servers" {
            self.servers_view(input, client).await?
        } else if resource == "stash" {
            self.stash_view(input)?
        } else if resource == "keybinds" {
            let view = crate::keybindings::view(input)?;
            self.keybindings = crate::keybindings::load()?;
            view
        } else {
            serde_json::from_value(client.manage(resource, input).await?)?
        };
        let mut view = view;
        if resource == "sharing" {
            for line in &mut view.lines {
                if line.starts_with("/s/") {
                    *line = format!(
                        "{}{line}",
                        client
                            .server_url()
                            .split("/v1/host/workspaces/")
                            .next()
                            .unwrap_or(&client.server_url())
                    );
                }
            }
        }
        self.management = Management {
            resource: resource.into(),
            view,
            ..Management::default()
        };
        self.show_management();
        Ok(())
    }
    pub(crate) fn show_management(&mut self) {
        self.management.details = false;
        let view = &self.management.view;
        let mut entries = Vec::new();
        for (index, field) in view.fields.iter().enumerate() {
            entries.push(OptionEntry {
                label: field.label.clone(),
                description: if field.secret && !field.value.is_empty() {
                    "••••••••".into()
                } else if field.value.is_empty() {
                    if field.required {
                        "Required".into()
                    } else {
                        "Optional".into()
                    }
                } else {
                    field.value.replace('\n', " ↵ ")
                },
                value: format!("field:{index}"),
            });
        }
        if view.submit.is_some() {
            entries.push(OptionEntry {
                label: if view
                    .submit
                    .as_ref()
                    .is_some_and(|request| request["action"] == "router_connect")
                {
                    "Connect"
                } else {
                    "Save"
                }
                .into(),
                description: "Apply these values".into(),
                value: "save".into(),
            });
        }
        for (index, entry) in view.entries.iter().enumerate() {
            entries.push(OptionEntry {
                label: entry.label.clone(),
                description: entry.description.clone(),
                value: format!("entry:{index}"),
            });
        }
        // The first lines of a view are printed above its choices; only a
        // long report needs a separate page to read the rest.
        if view.lines.len() > PREFACE_LINES {
            entries.push(OptionEntry {
                label: "Read everything".into(),
                description: format!("{} lines", view.lines.len()),
                value: "details".into(),
            });
        }
        entries.push(OptionEntry {
            label: "Back to manager".into(),
            description: String::new(),
            value: "back".into(),
        });
        self.open_overlay(Overlay::Options {
            title: view.title.clone(),
            kind: OptionsKind::Management,
            entries,
        });
    }
    pub(crate) fn management_secret(&self) -> bool {
        matches!(
            self.overlay,
            Overlay::Options {
                kind: OptionsKind::ManagementField,
                ..
            }
        ) && self
            .management
            .view
            .fields
            .get(self.management.field)
            .is_some_and(|f| f.secret)
    }
    pub(crate) async fn management_choice(
        &mut self,
        kind: OptionsKind,
        value: &str,
        client: &ApiClient,
    ) -> Result<()> {
        if matches!(
            kind,
            OptionsKind::ManagementField | OptionsKind::ManagementValue
        ) {
            let field = self
                .management
                .view
                .fields
                .get_mut(self.management.field)
                .context("Field no longer exists")?;
            if !field.choices.is_empty() && !field.choices.iter().any(|choice| choice == value) {
                anyhow::bail!("Choose one of the listed values");
            }
            field.value = value.into();
            self.show_management();
            self.selection = self.management.field;
            return Ok(());
        }
        if kind == OptionsKind::ManagementConfirm {
            if value == "confirm" {
                if let Some(request) = self.management.pending.take() {
                    let resource = self.management.resource.clone();
                    return self.open_management(&resource, &request, client).await;
                }
            }
            self.show_management();
            return Ok(());
        }
        let request = if value == "back" {
            json!({"action":"view"})
        } else if value == "details" {
            self.management.details = true;
            self.open_overlay(Overlay::Panel {
                title: self.management.view.title.clone(),
                lines: self
                    .management
                    .view
                    .lines
                    .iter()
                    .flat_map(|line| line.lines().map(str::to_owned))
                    .collect(),
            });
            return Ok(());
        } else if let Some(index) = value.strip_prefix("field:") {
            let index: usize = index.parse()?;
            let field = self
                .management
                .view
                .fields
                .get(index)
                .context("Field no longer exists")?
                .clone();
            self.management.field = index;
            self.open_overlay(Overlay::Options {
                title: field.label,
                kind: if field.choices.is_empty() {
                    OptionsKind::ManagementField
                } else {
                    OptionsKind::ManagementValue
                },
                entries: field
                    .choices
                    .iter()
                    .map(|v| OptionEntry {
                        label: v.clone(),
                        description: String::new(),
                        value: v.clone(),
                    })
                    .collect(),
            });
            if field.choices.is_empty() {
                self.overlay_filter = field.value;
            } else {
                self.selection = field
                    .choices
                    .iter()
                    .position(|v| v == &field.value)
                    .unwrap_or(0);
            }
            return Ok(());
        } else if value == "save" {
            let mut request = self
                .management
                .view
                .submit
                .clone()
                .context("Form is no longer available")?;
            for field in &self.management.view.fields {
                if field.required && field.value.trim().is_empty() {
                    self.notice = format!("{} is required", field.label);
                    self.show_management();
                    return Ok(());
                }
                request[&field.key] = json!(field.value);
            }
            request
        } else if let Some(index) = value.strip_prefix("entry:") {
            let entry = self
                .management
                .view
                .entries
                .get(index.parse::<usize>()?)
                .context("Action no longer exists")?
                .clone();
            if let Some(title) = entry.confirm {
                self.management.pending = Some(entry.request);
                self.open_overlay(Overlay::Options {
                    title,
                    kind: OptionsKind::ManagementConfirm,
                    entries: vec![
                        OptionEntry {
                            label: "Cancel".into(),
                            description: "Keep current settings".into(),
                            value: "cancel".into(),
                        },
                        OptionEntry {
                            label: entry.label,
                            description: entry.description,
                            value: "confirm".into(),
                        },
                    ],
                });
                return Ok(());
            }
            let mut request = entry.request;
            if request["action"] == "router_form" {
                for field in &self.management.view.fields {
                    if !field.secret {
                        request[&field.key] = json!(field.value);
                    }
                }
            }
            request
        } else {
            return Ok(());
        };
        let resource = self.management.resource.clone();
        // Retain a rejected form so validation/conflict errors never discard edits.
        if let Err(error) = self.open_management(&resource, &request, client).await {
            self.show_management();
            return Err(error);
        }
        Ok(())
    }
}
