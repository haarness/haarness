//! Immutable, revocable conversation snapshots hosted by the user's server.
use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Utc};
use haarness_protocol::{ItemData, SessionArchive, ThreadId};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::{Path as FsPath, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedMessage {
    pub role: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub token: String,
    pub thread_id: ThreadId,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub messages: Vec<SharedMessage>,
}
impl Snapshot {
    pub fn preview(archive: &SessionArchive, include_tools: bool) -> Result<Vec<SharedMessage>> {
        let thread = archive
            .threads
            .iter()
            .find(|t| t.thread.id == archive.root_thread_id)
            .context("Archive root missing")?;
        let messages = thread
            .items
            .iter()
            .filter_map(|item| match &item.data {
                ItemData::UserMessage {
                    text,
                    synthetic: false,
                    ..
                } => Some(SharedMessage {
                    role: "You".into(),
                    text: text.clone(),
                }),
                ItemData::AssistantMessage { text } => Some(SharedMessage {
                    role: "Assistant".into(),
                    text: text.clone(),
                }),
                ItemData::ToolCall {
                    name, arguments, ..
                } if include_tools => Some(SharedMessage {
                    role: format!("Tool · {name}"),
                    text: arguments.to_string(),
                }),
                ItemData::ToolResult { name, output, .. } if include_tools => Some(SharedMessage {
                    role: format!("Result · {name}"),
                    text: output.clone(),
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        ensure!(
            serde_json::to_vec(&messages)?.len() <= 4 * 1024 * 1024,
            "Snapshot exceeds 4 MiB; export the session archive instead"
        );
        Ok(messages)
    }
    pub fn create(
        root: &FsPath,
        archive: &SessionArchive,
        include_tools: bool,
        days: u32,
    ) -> Result<Self> {
        ensure!(
            (1..=30).contains(&days),
            "Shared links expire after 1–30 days"
        );
        let now = Utc::now();
        let snapshot = Self {
            token: format!(
                "{}{}",
                uuid::Uuid::now_v7().simple(),
                uuid::Uuid::now_v7().simple()
            ),
            thread_id: archive.root_thread_id,
            title: archive
                .threads
                .iter()
                .find(|t| t.thread.id == archive.root_thread_id)
                .context("Archive root missing")?
                .thread
                .title
                .clone(),
            created_at: now,
            expires_at: now + chrono::Duration::days(i64::from(days)),
            messages: Self::preview(archive, include_tools)?,
        };
        prune(root)?;
        let directory = root.join("shares");
        std::fs::create_dir_all(&directory)?;
        ensure!(
            std::fs::read_dir(&directory)?.take(1000).count() < 1000,
            "Share storage is full; revoke old links first"
        );
        haarness_profiles::write(
            &path(root, &snapshot.token)?,
            &serde_json::to_vec(&snapshot)?,
            false,
        )?;
        Ok(snapshot)
    }
    pub fn prepare(root: &FsPath, archive: &SessionArchive, include_tools: bool) -> Result<Self> {
        Self::create(&root.join("share-previews"), archive, include_tools, 7)
    }
    pub fn publish(root: &FsPath, token: &str) -> Result<Self> {
        if let Ok(existing) = Self::load(root, token) {
            return Ok(existing);
        }
        let preview_root = root.join("share-previews");
        let snapshot = Self::load(&preview_root, token)?;
        ensure!(
            Utc::now() - snapshot.created_at < chrono::Duration::minutes(10),
            "Preview expired; review the snapshot again"
        );
        prune(root)?;
        std::fs::create_dir_all(root.join("shares"))?;
        ensure!(
            std::fs::read_dir(root.join("shares"))?.take(1000).count() < 1000,
            "Share storage is full; revoke old links first"
        );
        haarness_profiles::write(&path(root, token)?, &serde_json::to_vec(&snapshot)?, false)?;
        Self::revoke(&preview_root, token)?;
        Ok(snapshot)
    }
    pub fn load(root: &FsPath, token: &str) -> Result<Self> {
        let snapshot: Self =
            serde_json::from_slice(&haarness_profiles::read(&path(root, token)?)?)?;
        ensure!(
            snapshot.token == token && snapshot.expires_at > Utc::now(),
            "Shared link expired"
        );
        Ok(snapshot)
    }
    pub fn revoke(root: &FsPath, token: &str) -> Result<()> {
        let file = path(root, token)?;
        haarness_profiles::check_path(&file)?;
        std::fs::remove_file(file)?;
        Ok(())
    }
    pub fn list(root: &FsPath, thread: ThreadId) -> Result<Vec<Self>> {
        let directory = root.join("shares");
        if !directory.exists() {
            return Ok(vec![]);
        }
        let mut values = std::fs::read_dir(directory)?
            .take(1000)
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| {
                entry
                    .path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_owned)
            })
            .filter_map(|token| Self::load(root, &token).ok())
            .filter(|row| row.thread_id == thread)
            .collect::<Vec<_>>();
        values.sort_by_key(|row| std::cmp::Reverse(row.created_at));
        Ok(values)
    }
}
fn prune(root: &FsPath) -> Result<()> {
    let directory = root.join("shares");
    if !directory.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)?.take(2000).flatten() {
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(token) = entry
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        let Ok(file) = path(root, &token) else {
            continue;
        };
        let Ok(bytes) = haarness_profiles::read(&file) else {
            continue;
        };
        let Ok(snapshot) = serde_json::from_slice::<Snapshot>(&bytes) else {
            continue;
        };
        let preview = root.file_name().is_some_and(|v| v == "share-previews");
        if snapshot.expires_at <= Utc::now()
            || (preview && Utc::now() - snapshot.created_at > chrono::Duration::minutes(10))
        {
            Snapshot::revoke(root, &token)?;
        }
    }
    Ok(())
}
fn path(root: &FsPath, token: &str) -> Result<PathBuf> {
    ensure!(
        token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid shared link"
    );
    Ok(root.join("shares").join(format!("{token}.json")))
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn html(snapshot: &Snapshot) -> String {
    let mut page = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>body{{font:16px system-ui;line-height:1.6;max-width:850px;margin:48px auto;padding:0 24px;color:#dedbd4;background:#181816}}h1{{font-size:28px}}h2{{font-size:14px;color:#aaa}}article{{border-top:1px solid #444;padding:18px 0}}pre{{white-space:pre-wrap;overflow-wrap:anywhere;font:inherit}}footer{{color:#aaa;font-size:13px}}</style><h1>{}</h1><p>Shared conversation snapshot</p>",
        escape(&snapshot.title),
        escape(&snapshot.title)
    );
    for message in &snapshot.messages {
        let _ = write!(
            page,
            "<article><h2>{}</h2><pre>{}</pre></article>",
            escape(&message.role),
            escape(&message.text)
        );
    }
    let _ = write!(
        page,
        "<footer>Shared {} · expires {}</footer></html>",
        snapshot.created_at.format("%Y-%m-%d"),
        snapshot.expires_at.format("%Y-%m-%d")
    );
    page
}
pub(crate) fn routes() -> Router<crate::ServerState> {
    Router::new().route("/s/{token}", get(read))
}
async fn read(State(state): State<crate::ServerState>, Path(token): Path<String>) -> Response {
    let Some(manager) = state.workspaces.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let root = manager.data_directory().to_path_buf();
    let snapshot = tokio::task::spawn_blocking(move || Snapshot::load(&root, &token)).await;
    match snapshot {
        Ok(Ok(snapshot)) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
                ),
                (header::REFERRER_POLICY, "no-referrer"),
                (header::CACHE_CONTROL, "no-store"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            html(&snapshot),
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_html_never_executes_conversation_markup() {
        let row = Snapshot {
            token: "a".repeat(64),
            thread_id: ThreadId::new(),
            title: "<script>bad</script>".into(),
            created_at: Utc::now(),
            expires_at: Utc::now(),
            messages: vec![SharedMessage {
                role: "You".into(),
                text: "<img src=x onerror=alert(1)>".into(),
            }],
        };
        let page = html(&row);
        assert!(!page.contains("<script>"));
        assert!(!page.contains("<img"));
        assert!(page.contains("&lt;img"));
        assert!(path(FsPath::new("/tmp"), "../auth").is_err());
    }
}
