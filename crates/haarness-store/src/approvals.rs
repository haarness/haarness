//! Repository consent lives outside workspace databases; linked worktrees share it.
use haarness_core::{
    CoreError, CoreResult, PermissionDecision, PermissionRule, RepositoryApprovalStore,
};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::path::Path;

pub struct RepositoryApprovals {
    identity: String,
    connection: Mutex<Connection>,
}
impl RepositoryApprovals {
    pub fn open(path: &Path, canonical_git_common_dir: &Path) -> CoreResult<Self> {
        let error = |e: &dyn std::fmt::Display| CoreError::Storage(e.to_string());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| error(&e))?;
        }
        let connection = Connection::open(path).map_err(|e| error(&e))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| error(&e))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS grants(repository TEXT NOT NULL, pattern TEXT NOT NULL, PRIMARY KEY(repository,pattern));").map_err(|e| error(&e))?;
        Ok(Self {
            identity: canonical_git_common_dir.display().to_string(),
            connection: Mutex::new(connection),
        })
    }
}
impl RepositoryApprovalStore for RepositoryApprovals {
    fn identity(&self) -> &str {
        &self.identity
    }
    fn load(&self) -> CoreResult<Vec<PermissionRule>> {
        let connection = self.connection.lock();
        let mut query = connection
            .prepare("SELECT pattern FROM grants WHERE repository=?1 ORDER BY pattern")
            .map_err(storage)?;
        query
            .query_map([&self.identity], |row| {
                Ok(PermissionRule {
                    pattern: row.get(0)?,
                    decision: PermissionDecision::Allow,
                })
            })
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)
    }
    fn save(&self, rules: &[PermissionRule]) -> CoreResult<()> {
        let mut connection = self.connection.lock();
        let tx = connection.transaction().map_err(storage)?;
        for rule in rules {
            if rule.decision != PermissionDecision::Allow {
                return Err(CoreError::Invalid(
                    "repository grants can only allow".into(),
                ));
            }
            tx.execute(
                "INSERT OR IGNORE INTO grants(repository,pattern) VALUES(?1,?2)",
                params![self.identity, rule.pattern],
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)
    }
}
fn storage(error: rusqlite::Error) -> CoreError {
    CoreError::Storage(error.to_string())
}
