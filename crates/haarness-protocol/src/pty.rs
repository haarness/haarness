use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Server-owned pseudo-terminal identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct PtyId(pub Uuid);

impl PtyId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for PtyId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PtyId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PtyStatus {
    Running,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PtyInfo {
    pub id: PtyId,
    pub title: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub status: PtyStatus,
    pub pid: Option<u32>,
    pub cols: u16,
    pub rows: u16,
    /// True only in the answer to the create that started this terminal, so
    /// a client can tell a shell it started (whose output is all new to it)
    /// from one an `ifNone` create handed back. Always sent, so a client can
    /// tell `false` from a server too old to say (whose create always starts
    /// one).
    #[serde(default)]
    pub created: bool,
    /// The chat folder this terminal was opened for
    /// ([`PtyCreateRequest::directory`]), canonical and absolute. Absent for
    /// a terminal of the project itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PtyCreateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
    /// Start a terminal only when the project has none running; otherwise
    /// return its oldest running one. Several windows opening "a terminal" at
    /// once (after the server was replaced, say) then share one shell instead
    /// of starting one each.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub if_none: bool,
    /// Open the terminal for a chat's folder (a thread's `directory`): given
    /// relative to the project folder, as a chat's folder is created, or as
    /// the absolute folder the thread reports. It must exist inside the
    /// project; the shell starts there, and `ifNone` then hands back only a
    /// terminal opened for the same folder (without it, only one opened
    /// without a folder). Not together with `cwd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

const fn default_cols() -> u16 {
    80
}

const fn default_rows() -> u16 {
    24
}

impl Default for PtyCreateRequest {
    fn default() -> Self {
        Self {
            title: None,
            command: None,
            args: Vec::new(),
            cwd: None,
            cols: default_cols(),
            rows: default_rows(),
            if_none: false,
            directory: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PtyUpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
}

/// Text control frame sent before replay and whenever the output cursor moves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PtyCursor {
    /// Where the replay that follows starts. A truncated replay starts at the
    /// first whole line (or character) after the oldest byte still buffered.
    pub start: u64,
    pub end: u64,
    /// The client asked for output older than the buffer holds.
    pub truncated: bool,
    /// On a truncated replay, the escape sequences that restore the terminal
    /// modes the dropped output left on (alternate screen, application cursor
    /// keys and keypad, mouse reporting, bracketed paste, a hidden cursor…),
    /// for a client to apply to a reset terminal before the replay. Absent
    /// when there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_always_says_whether_its_create_started_it() {
        let info = PtyInfo {
            id: PtyId::new(),
            title: "zsh".into(),
            command: "zsh".into(),
            args: Vec::new(),
            cwd: "/tmp".into(),
            status: PtyStatus::Running,
            pid: None,
            cols: 80,
            rows: 24,
            created: false,
            directory: None,
        };
        let sent = serde_json::to_value(&info).unwrap();
        assert_eq!(sent["created"], false);
        // An older server says nothing; it reads as false here.
        let mut older = sent;
        older.as_object_mut().unwrap().remove("created");
        assert!(!serde_json::from_value::<PtyInfo>(older).unwrap().created);
    }
}
