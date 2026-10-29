//! Server-scoped configuration forms shared by interactive clients.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManagementView {
    pub title: String,
    /// A square image drawn beside the title, as an absolute https URL.
    /// Clients that show text only leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Structured resource state for clients that render their own controls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default)]
    pub lines: Vec<String>,
    #[serde(default)]
    pub entries: Vec<ManagementEntry>,
    #[serde(default)]
    pub fields: Vec<ManagementField>,
    #[serde(default)]
    pub submit: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManagementEntry {
    pub label: String,
    pub description: String,
    pub request: Value,
    #[serde(default)]
    pub confirm: Option<String>,
    /// A square image drawn beside the label, as an absolute https URL.
    /// Clients that show text only leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManagementField {
    pub key: String,
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
    /// Empty for free text, otherwise an enumerated choice.
    #[serde(default)]
    pub choices: Vec<String>,
}
