//! Discover a missing launcher without changing the host process environment.
use haarness_protocol::McpConfigureRequest;
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

pub(super) fn resolve(program: &str, config: &McpConfigureRequest) -> Option<(PathBuf, OsString)> {
    if cfg!(windows)
        || program.contains(['/', '\\'])
        || config.env.contains_key("PATH")
        || config.env.contains_key("Path")
    {
        return None;
    }
    let cwd = config.cwd.as_ref().map_or_else(
        || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        PathBuf::from,
    );
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let home = config
        .env
        .get("HOME")
        .map(|value| OsString::from(super::expand_env_reference(value)))
        .or_else(|| haarness_protocol::home_dir().map(OsString::from));
    let extra = config
        .env
        .get("HAARNESS_MCP_PATH")
        .map(|value| OsString::from(super::expand_env_reference(value)))
        .or_else(|| std::env::var_os("HAARNESS_MCP_PATH"));
    fallback_path(program, &cwd, &inherited, home.as_deref(), extra.as_deref())
}

fn fallback_path(
    program: &str,
    cwd: &Path,
    inherited: &OsStr,
    home: Option<&OsStr>,
    extra: Option<&OsStr>,
) -> Option<(PathBuf, OsString)> {
    let original = std::env::split_paths(inherited)
        .map(|path| cwd.join(path))
        .collect::<Vec<_>>();
    if locate(program, &original).is_some() {
        return None;
    }
    let mut directories = extra
        .map(|value| {
            std::env::split_paths(value)
                .filter(|path| path.is_absolute())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(home) = home {
        let home = Path::new(home);
        if home.is_absolute() {
            directories.extend([
                home.join(".local/bin"),
                home.join(".cargo/bin"),
                home.join(".bun/bin"),
            ]);
        }
    }
    if cfg!(target_os = "macos") {
        directories.push(PathBuf::from("/opt/homebrew/bin"));
    }
    directories.push(PathBuf::from("/usr/local/bin"));
    let executable = locate(program, &directories)?;
    let parent = executable.parent()?.to_path_buf();
    let mut paths = std::env::split_paths(inherited).collect::<Vec<_>>();
    // Preserve inherited precedence, then make the launcher's runtime (for
    // example node for npx) visible to its child processes as well.
    for path in std::iter::once(parent).chain(directories) {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Some((executable, std::env::join_paths(paths).ok()?))
}

fn locate(program: &str, directories: &[PathBuf]) -> Option<PathBuf> {
    directories
        .iter()
        .map(|path| path.join(program))
        .find(|path| {
            let Ok(metadata) = path.metadata() else {
                return false;
            };
            if !metadata.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[tokio::test]
    async fn missing_launcher_and_its_interpreter_are_discovered_without_replacing_existing_commands()
     {
        let root =
            std::env::temp_dir().join(format!("haarness-executable-{}", uuid::Uuid::now_v7()));
        let directory = root.join(".local/bin");
        let installed = root.join("installed");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::create_dir_all(&installed).unwrap();
        let name = "haarness-fixture-launcher";
        write(
            &directory.join(name),
            "#!/usr/bin/env haarness-fixture-interpreter\n",
        );
        write(
            &directory.join("haarness-fixture-interpreter"),
            "#!/bin/sh\nprintf 'discovered-runtime'\n",
        );
        let (path, env) = fallback_path(
            name,
            &root,
            installed.as_os_str(),
            Some(root.as_os_str()),
            None,
        )
        .unwrap();
        assert_eq!(path, directory.join(name));
        let output = tokio::process::Command::new(path)
            .env("PATH", env)
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"discovered-runtime");
        write(&installed.join(name), "#!/bin/sh\nexit 17\n");
        assert!(
            fallback_path(
                name,
                &root,
                installed.as_os_str(),
                Some(root.as_os_str()),
                None
            )
            .is_none()
        );
        assert!(
            fallback_path(
                "not-installed-anywhere",
                &root,
                installed.as_os_str(),
                None,
                Some(OsStr::new("relative/fallback"))
            )
            .is_none()
        );
        let mut config: McpConfigureRequest = serde_json::from_value(serde_json::json!({"transport":"stdio","command":name,"env":{"HOME":root},"enabled":true})).unwrap();
        assert!(resolve(name, &config).is_some());
        config
            .env
            .insert("PATH".into(), "/explicit/missing-path".into());
        assert!(resolve(name, &config).is_none());
        config.env.remove("PATH");
        assert!(resolve("./haarness-fixture-launcher", &config).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
