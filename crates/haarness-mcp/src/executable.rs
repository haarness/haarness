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
    let setting = |name: &str| {
        config
            .env
            .get(name)
            .map(|value| OsString::from(super::expand_env_reference(value)))
            .or_else(|| std::env::var_os(name))
    };
    let extra = setting("HAARNESS_MCP_PATH");
    let nvm = setting("NVM_DIR");
    fallback_path(
        program,
        &cwd,
        &inherited,
        home.as_deref(),
        extra.as_deref(),
        nvm.as_deref(),
        &system_directories(),
    )
}

/// The system's own folders of installed programs.
fn system_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if cfg!(target_os = "macos") {
        directories.push(PathBuf::from("/opt/homebrew/bin"));
    }
    directories.push(PathBuf::from("/usr/local/bin"));
    directories
}

fn fallback_path(
    program: &str,
    cwd: &Path,
    inherited: &OsStr,
    home: Option<&OsStr>,
    extra: Option<&OsStr>,
    nvm: Option<&OsStr>,
    system: &[PathBuf],
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
    let home = home.map(Path::new).filter(|home| home.is_absolute());
    if let Some(home) = home {
        directories.extend([
            home.join(".local/bin"),
            home.join(".cargo/bin"),
            home.join(".bun/bin"),
        ]);
    }
    directories.extend(system.iter().cloned());
    // The version managers' folders come last: they help only where nothing
    // above has the launcher, so a server started without a terminal's PATH
    // (a launchd agent, `ssh host haarness serve`) keeps the Homebrew npx it
    // found before over a stale nvm version or a shim with no node chosen.
    if let Some(home) = home {
        let nvm = nvm
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".nvm"));
        directories.extend(node_managers(home, &nvm));
    }
    // Only folders that are there reach the launcher's PATH.
    directories.retain(|path| path.is_dir());
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

/// Where Node version managers keep the launchers a new terminal would find
/// (`npx`, `node`, global packages), for an app opened outside a shell, which
/// never ran the profile that puts them on PATH. Each keeps node beside npx,
/// so a launcher found here also finds the runtime its `#!/usr/bin/env node`
/// asks for. Nothing here runs a program or walks a tree: at most the folder
/// of nvm's installed versions is listed.
fn node_managers(home: &Path, nvm: &Path) -> Vec<PathBuf> {
    let mut directories = vec![
        home.join(".volta/bin"),
        home.join(".asdf/shims"),
        home.join(".local/share/mise/shims"),
    ];
    let mut fnm = vec![home.join(".local/share/fnm"), home.join(".fnm")];
    if cfg!(target_os = "macos") {
        fnm.push(home.join("Library/Application Support/fnm"));
    }
    directories.extend(fnm.iter().map(|root| root.join("aliases/default/bin")));
    directories.extend(nvm_directories(nvm));
    directories.push(home.join(".local/share/pnpm"));
    if cfg!(target_os = "macos") {
        directories.push(home.join("Library/pnpm"));
    }
    directories.push(home.join(".yarn/bin"));
    directories
}

/// The node a new terminal gets from nvm: its `current` link when it keeps
/// one, then the version its `default` alias names (`18`, `v20.11`, an exact
/// version, or an alias such as `lts/*` that names one of those). `node`,
/// an alias that names nothing installed, or no alias at all give the newest
/// installed version; `system` gives none.
fn nvm_directories(root: &Path) -> Vec<PathBuf> {
    // Enough for every version anyone keeps; a folder past it is not read.
    const MAX_VERSIONS: usize = 512;
    let mut directories = vec![root.join("current/bin")];
    let installed = root.join("versions/node");
    let Ok(entries) = std::fs::read_dir(&installed) else {
        return directories;
    };
    let mut versions: Vec<(Vec<u64>, String)> = entries
        .take(MAX_VERSIONS)
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| Some((version_numbers(name.strip_prefix('v')?)?, name)))
        .collect();
    versions.sort_by(|left, right| right.0.cmp(&left.0));
    let alias = nvm_default(root);
    if alias.as_deref() == Some("system") {
        return directories;
    }
    let wanted = alias
        .as_deref()
        .map(|alias| alias.strip_prefix('v').unwrap_or(alias))
        .and_then(version_numbers);
    let picked = wanted
        .and_then(|wanted| {
            versions
                .iter()
                .find(|(numbers, _)| numbers.starts_with(&wanted))
        })
        .or_else(|| versions.first());
    if let Some((_, name)) = picked {
        directories.push(installed.join(name).join("bin"));
    }
    directories
}

/// What nvm's `default` alias leads to after following the aliases it
/// names (`lts/*` → `lts/iron` → `v20.18.1`), a few at most.
fn nvm_default(root: &Path) -> Option<String> {
    let aliases = root.join("alias");
    let read = |name: &str| {
        // An alias is a name inside the alias folder, never a path out of it.
        if name.is_empty() || name.split('/').any(|part| part.is_empty() || part == "..") {
            return None;
        }
        let text = std::fs::read_to_string(aliases.join(name)).ok()?;
        Some(text.trim().to_owned()).filter(|text| !text.is_empty())
    };
    let mut value = read("default")?;
    for _ in 0..4 {
        match read(&value) {
            Some(next) => value = next,
            None => break,
        }
    }
    Some(value)
}

/// `20.18.1` (or a part of one, such as `20` or `20.18`) as numbers.
fn version_numbers(text: &str) -> Option<Vec<u64>> {
    text.split('.')
        .map(|part| part.parse().ok())
        .collect::<Option<Vec<u64>>>()
        .filter(|numbers| !numbers.is_empty())
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
            None,
            &[],
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
                None,
                None,
                &[]
            )
            .is_none()
        );
        assert!(
            fallback_path(
                "not-installed-anywhere",
                &root,
                installed.as_os_str(),
                None,
                Some(OsStr::new("relative/fallback")),
                None,
                &[]
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

    /// A fake home folder, removed when the test ends.
    struct Home(PathBuf);

    impl Home {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("haarness-node-home-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        /// An executable `name` in `folder` (relative to the home folder).
        fn launcher(&self, folder: &str, name: &str) -> PathBuf {
            let directory = self.0.join(folder);
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join(name);
            write(&path, "#!/bin/sh\n");
            path
        }

        fn file(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        fn find(&self, program: &str, nvm: Option<&Path>) -> Option<(PathBuf, OsString)> {
            self.find_with(program, nvm, &[])
        }

        /// As `find`, with `system` as the system's own program folders.
        fn find_with(
            &self,
            program: &str,
            nvm: Option<&Path>,
            system: &[PathBuf],
        ) -> Option<(PathBuf, OsString)> {
            fallback_path(
                program,
                &self.0,
                OsStr::new("/usr/bin:/bin"),
                Some(self.0.as_os_str()),
                None,
                nvm.map(Path::as_os_str),
                system,
            )
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn launchers_of_node_version_managers_are_found_after_the_inherited_path() {
        let mut folders = vec![
            ".volta/bin",
            ".asdf/shims",
            ".local/share/mise/shims",
            ".local/share/fnm/aliases/default/bin",
            ".fnm/aliases/default/bin",
            ".nvm/current/bin",
            ".nvm/versions/node/v20.0.0/bin",
            ".local/share/pnpm",
            ".yarn/bin",
        ];
        if cfg!(target_os = "macos") {
            folders.extend([
                "Library/Application Support/fnm/aliases/default/bin",
                "Library/pnpm",
            ]);
        }
        for folder in folders {
            let home = Home::new();
            let launcher = home.launcher(folder, "haarness-fixture-npx");
            let (path, env) = home.find("haarness-fixture-npx", None).expect(folder);
            assert_eq!(path, launcher, "{folder}");
            let env = std::env::split_paths(&env).collect::<Vec<_>>();
            assert_eq!(
                &env[..3],
                [
                    PathBuf::from("/usr/bin"),
                    PathBuf::from("/bin"),
                    launcher.parent().unwrap().to_path_buf()
                ],
                "{folder}"
            );
            // Folders that are not there stay off the launcher's PATH.
            assert!(
                env.iter()
                    .all(|path| path.is_dir() || !path.starts_with(&home.0)),
                "{folder}: {env:?}"
            );
        }
    }

    #[test]
    fn nvm_gives_the_version_a_new_terminal_would_get() {
        /// A case's name, the alias files it writes, and the version it gets.
        type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
        let installed = ["v8.17.0", "v18.20.0", "v20.18.1", "v22.9.0", "v22.11.0"];
        let cases: &[Case] = &[
            ("no default alias", &[], "v22.11.0"),
            ("a major version", &[("default", "18")], "v18.20.0"),
            ("a minor version with v", &[("default", "v22.9")], "v22.9.0"),
            ("an exact version", &[("default", "v22.9.0\n")], "v22.9.0"),
            (
                "an lts alias chain",
                &[
                    ("default", "lts/*"),
                    ("lts/*", "lts/iron"),
                    ("lts/iron", "v20.18.1"),
                ],
                "v20.18.1",
            ),
            ("node", &[("default", "node")], "v22.11.0"),
            ("a version not installed", &[("default", "16")], "v22.11.0"),
            (
                "an alias that names nothing",
                &[("default", "lts/*")],
                "v22.11.0",
            ),
            (
                "an alias leading out of its folder",
                &[("default", "../../x")],
                "v22.11.0",
            ),
        ];
        for (case, aliases, expected) in cases {
            let home = Home::new();
            for version in installed {
                home.launcher(
                    &format!(".nvm/versions/node/{version}/bin"),
                    "haarness-fixture-npx",
                );
            }
            for (name, value) in *aliases {
                home.file(&format!(".nvm/alias/{name}"), value);
            }
            let (path, env) = home.find("haarness-fixture-npx", None).expect(case);
            assert_eq!(
                path,
                home.0.join(format!(
                    ".nvm/versions/node/{expected}/bin/haarness-fixture-npx"
                )),
                "{case}"
            );
            // One installed version reaches the PATH, never all of them.
            let versions = std::env::split_paths(&env)
                .filter(|path| path.starts_with(home.0.join(".nvm/versions")))
                .count();
            assert_eq!(versions, 1, "{case}");
        }
    }

    #[test]
    fn nvm_current_link_and_folder_setting_come_first_and_system_node_leaves_nvm_out() {
        let home = Home::new();
        home.launcher(
            "custom nvm/versions/node/v22.11.0/bin",
            "haarness-fixture-npx",
        );
        let current = home.launcher("custom nvm/current/bin", "haarness-fixture-npx");
        home.launcher(".nvm/versions/node/v23.0.0/bin", "haarness-fixture-npx");
        let custom = home.0.join("custom nvm");
        assert_eq!(
            home.find("haarness-fixture-npx", Some(&custom)).unwrap().0,
            current
        );
        // A relative folder setting is not used; the default folder is.
        assert_eq!(
            home.find("haarness-fixture-npx", Some(Path::new("custom nvm")))
                .unwrap()
                .0,
            home.0
                .join(".nvm/versions/node/v23.0.0/bin/haarness-fixture-npx")
        );

        let system = Home::new();
        system.launcher(".nvm/versions/node/v22.11.0/bin", "haarness-fixture-npx");
        system.file(".nvm/alias/default", "system");
        assert!(system.find("haarness-fixture-npx", None).is_none());
    }

    /// A launcher in the system's own folders (Homebrew's) is the one a
    /// server without a terminal's PATH found before; a version manager's
    /// stale copy never takes its place.
    #[test]
    fn the_systems_launcher_comes_before_a_version_managers() {
        let home = Home::new();
        let homebrew = home.launcher("opt/homebrew/bin", "haarness-fixture-npx");
        for stale in [
            ".nvm/versions/node/v14.21.3/bin",
            ".volta/bin",
            ".asdf/shims",
        ] {
            home.launcher(stale, "haarness-fixture-npx");
        }
        let system = [homebrew.parent().unwrap().to_path_buf()];
        let (path, env) = home
            .find_with("haarness-fixture-npx", None, &system)
            .unwrap();
        assert_eq!(path, homebrew);
        // The managers' folders still follow, after the system's.
        let env = std::env::split_paths(&env).collect::<Vec<_>>();
        let at = |folder: &str| env.iter().position(|path| path == &home.0.join(folder));
        assert!(at("opt/homebrew/bin") < at(".volta/bin"), "{env:?}");
    }
}
