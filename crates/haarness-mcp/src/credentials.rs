//! Shared credential storage and durable logout fencing across workspace processes.
use async_trait::async_trait;
use fs2::FileExt;
use rmcp::transport::{AuthError, CredentialStore, StoredCredentials};
use std::path::PathBuf;

#[derive(Clone)]
pub(super) struct FileCredentialStore {
    pub(super) path: PathBuf,
    epoch: Result<Vec<u8>, ()>,
}
impl FileCredentialStore {
    pub(super) fn new(path: PathBuf) -> Self {
        let epoch = Self::read_epoch(&path);
        Self { path, epoch }
    }
    fn read_epoch(path: &std::path::Path) -> Result<Vec<u8>, ()> {
        match std::fs::read(path.with_extension("revoked")) {
            Ok(value) => Ok(value),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err(()),
        }
    }
    pub(super) fn valid(&self) -> bool {
        self.epoch.is_ok() && Self::read_epoch(&self.path) == self.epoch
    }
    fn check(&self) -> Result<(), AuthError> {
        if self.valid() {
            Ok(())
        } else {
            Err(AuthError::InternalError(
                "MCP OAuth credentials were cleared; reconnect after signing in".into(),
            ))
        }
    }
    fn lock(&self) -> Result<std::fs::File, AuthError> {
        let parent = self.path.parent().ok_or_else(|| {
            AuthError::InternalError("MCP OAuth credential path has no parent".into())
        })?;
        std::fs::create_dir_all(parent).map_err(store_error)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_extension("lock"))
            .map_err(store_error)?;
        set_private(&file).map_err(store_error)?;
        file.lock_exclusive().map_err(store_error)?;
        Ok(file)
    }
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let _lock = self.lock()?;
        self.check()?;
        match std::fs::read(&self.path) {
            Ok(body) => serde_json::from_slice(&body).map(Some).map_err(|error| {
                AuthError::InternalError(format!(
                    "stored MCP OAuth credentials are invalid: {error}"
                ))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(store_error(error)),
        }
    }
    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let _lock = self.lock()?;
        // Token refresh and OAuth callbacks must not restore an account after logout.
        self.check()?;
        let body = serde_json::to_vec_pretty(&credentials).map_err(|error| {
            AuthError::InternalError(format!("cannot serialize MCP OAuth credentials: {error}"))
        })?;
        atomic_write(&self.path, &body)
    }
    async fn clear(&self) -> Result<(), AuthError> {
        let _lock = self.lock()?;
        // Persist the fence before removing tokens. Other processes and pending
        // refreshes observe the same epoch even after a later sign-in.
        atomic_write(
            &self.path.with_extension("revoked"),
            uuid::Uuid::now_v7().to_string().as_bytes(),
        )?;
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(store_error(error)),
        }
    }
}
fn atomic_write(path: &std::path::Path, body: &[u8]) -> Result<(), AuthError> {
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::now_v7()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        set_private(&file)?;
        std::io::Write::write_all(&mut file, body)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)]
        std::fs::File::open(path.parent().expect("credential parent"))?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(store_error)
}
fn store_error(error: std::io::Error) -> AuthError {
    AuthError::InternalError(format!(
        "cannot update stored MCP OAuth credentials: {error}"
    ))
}
#[cfg(unix)]
fn set_private(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}
#[cfg(not(unix))]
fn set_private(_file: &std::fs::File) -> std::io::Result<()> {
    Ok(())
}
