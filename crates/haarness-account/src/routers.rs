//! Local router preference, shared by Desktop/TUI/runtime and isolated by issuer/account.
use crate::{Client, Config, Error, Result};
use reqwest::Method;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Write};

impl Config {
    fn router_scope(&self) -> String {
        let identity = self
            .api_key()
            .map(|key| format!("key:{:x}", Sha256::digest(key.as_bytes())))
            .unwrap_or_else(|| {
                self.selected_account()
                    .map(|id| format!("account:{id}"))
                    .unwrap_or_else(|| {
                        format!(
                            "default:{}",
                            fs::read_to_string(self.config_dir.join("login-generation"))
                                .unwrap_or_default()
                        )
                    })
            });
        format!("{}:{identity}", self.api_url)
    }
    pub fn selected_router(&self) -> Result<Option<String>> {
        let path = self.config_dir.join("routers.json");
        let data = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let object = data.as_object().ok_or_else(|| {
            Error::Config("Invalid router preferences; select a router again".into())
        })?;
        let Some(value) = object.get(&self.router_scope()) else {
            return Ok(None);
        };
        let id = value
            .as_str()
            .ok_or_else(|| Error::Config("Invalid saved Cloud router ID".into()))?;
        uuid::Uuid::parse_str(id).map_err(|_| {
            Error::Config("Saved Cloud router ID is invalid; select a router again".into())
        })?;
        Ok(Some(id.into()))
    }
}
impl Client {
    pub async fn routers(&self) -> Result<Value> {
        let mut path = String::from("api/v1/routers");
        if let Some(account) = self.config.selected_account() {
            uuid::Uuid::parse_str(&account)
                .map_err(|_| Error::Config("Invalid account ID".into()))?;
            path.push_str(&format!("?accountId={account}"));
        }
        self.request(Method::GET, &path, None).await
    }
    pub async fn select_router(&self, id: &str) -> Result<Value> {
        uuid::Uuid::parse_str(id).map_err(|_| Error::Config("Invalid Cloud router ID".into()))?;
        let scope = self.config.router_scope();
        let routers = self.routers().await?;
        let row = routers["items"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == id))
            .ok_or_else(|| Error::Config("Router unavailable for the selected account".into()))?
            .clone();
        let _lock = self.config.lock().await?;
        if self.config.router_scope() != scope {
            return Err(Error::Config(
                "Cloud account changed; select the router again".into(),
            ));
        }
        // Bind an implicit default account before saving, so a later login cannot inherit it.
        if self.config.api_key().is_none() && self.config.selected_account().is_none() {
            let account = row["accountId"]
                .as_str()
                .ok_or_else(|| Error::Config("Router has no account".into()))?;
            uuid::Uuid::parse_str(account)
                .map_err(|_| Error::Config("Invalid router account".into()))?;
            save_preference(
                &self.config.config_dir.join("account.json"),
                &json!({"apiUrl":self.config.api_url,"accountId":account}),
            )?;
        }
        let path = self.config.config_dir.join("routers.json");
        let mut value = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
            Err(e) => return Err(e.into()),
        };
        value
            .as_object_mut()
            .ok_or_else(|| Error::Config("Invalid router preferences".into()))?
            .insert(self.config.router_scope(), json!(id));
        save_preference(&path, &value)?;
        Ok(row)
    }
}

fn save_preference(path: &std::path::Path, value: &Value) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::now_v7()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&serde_json::to_vec(&value)?)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
