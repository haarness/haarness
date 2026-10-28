//! `--data-dir` holds everything a run writes, the server's own files too:
//! a test or an isolated install that names one never writes into the app
//! data folder of the person's home.
use std::{fs, path::Path, process::Command};

fn haarness(root: &Path, args: &[&str]) -> std::process::Output {
    haarness_with(root, args, &[])
}

fn haarness_with(root: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let home = root.join("home");
    Command::new(env!("CARGO_BIN_EXE_haarness"))
        .args([
            "--data-dir",
            root.join("data").to_str().unwrap(),
            "--workspace",
            root.join("workspace").to_str().unwrap(),
        ])
        .args(args)
        .current_dir(root.join("workspace"))
        .env_remove("HAARNESS_DATA_DIR")
        .env_remove("HAARNESS_HOME")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("HAARNESS_TEST_PROVIDER", "1")
        .env("HAARNESS_TELEMETRY", "0")
        .env("HAARNESS_CLOUD_API_URL", "http://127.0.0.1:9")
        .env("RUST_LOG", "warn")
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

#[test]
fn a_run_with_a_data_dir_saves_long_tool_output_inside_it() {
    let root = std::env::temp_dir().join(format!("haarness-data-dir-{}", uuid::Uuid::now_v7()));
    for folder in ["home", "workspace", "data"] {
        fs::create_dir_all(root.join(folder)).unwrap();
    }
    let root = root.canonicalize().unwrap();
    // Output too long to keep in the conversation is saved to a file.
    let output = haarness(
        &root,
        &[
            "run",
            "--yolo",
            "--model",
            "echo/echo-1",
            r#"!tool bash {"command":"seq 1 300000"}"#,
        ],
    );
    let _ = haarness(&root, &["serve", "--stop"]);
    assert!(output.status.success(), "{output:?}");
    let saved: Vec<_> = fs::read_dir(root.join("data/data/tool-output"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    assert_eq!(saved.len(), 1, "{saved:?}");
    // The whole output is the person's alone, and so is the data folder.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&saved[0]), 0o600);
        assert_eq!(mode(&root.join("data/data/tool-output")), 0o700);
        assert_eq!(mode(&root.join("data/data")), 0o700);
    }
    // Nothing went to the app data folder of the home.
    for folder in [
        "home/Library/Application Support/dev.haarness.haarness",
        "home/.local/share/dev.haarness.haarness",
        "home/.local/share/haarness",
    ] {
        assert!(!root.join(folder).join("tool-output").exists(), "{folder}");
    }
    fs::remove_dir_all(root).unwrap();
}

/// `--data-dir ~` makes the person's own `~/data` the data folder: a run
/// leaves it open as it was until Haarness's files are kept there.
#[cfg(unix)]
#[test]
fn a_folder_of_the_person_s_named_as_the_data_folder_is_left_as_it_is() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = std::env::temp_dir().join(format!("haarness-theirs-{}", uuid::Uuid::now_v7()));
    // `haarness` names `<root>/data` as the root, so its data folder is
    // `<root>/data/data`.
    let theirs = root.join("data/data");
    for folder in [root.join("home"), root.join("workspace"), theirs.clone()] {
        fs::create_dir_all(folder).unwrap();
    }
    fs::write(theirs.join("photo.jpg"), "").unwrap();
    fs::set_permissions(&theirs, fs::Permissions::from_mode(0o755)).unwrap();
    let _ = haarness(&root, &["logs"]);
    let mode = fs::metadata(&theirs).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
    fs::remove_dir_all(root).unwrap();
}

/// Without `--data-dir`, a Mac's server keeps saved output and previews in
/// the app's Application Support folder, which need not be the data folder
/// (`~/.local/share/haarness`): one an older build left open, with
/// Haarness's files in it, is narrowed at every start as the data folder is.
#[cfg(target_os = "macos")]
#[test]
fn the_server_s_own_folder_is_narrowed_at_start_too() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = std::env::temp_dir().join(format!("haarness-server-dir-{}", uuid::Uuid::now_v7()));
    let home = root.join("home");
    let server = home.join("Library/Application Support/dev.haarness.haarness");
    fs::create_dir_all(server.join("tool-output")).unwrap();
    fs::set_permissions(&server, fs::Permissions::from_mode(0o755)).unwrap();
    // A data folder of its own, so the server's is not taken over as it.
    fs::create_dir_all(home.join(".local/share/haarness")).unwrap();
    let _ = Command::new(env!("CARGO_BIN_EXE_haarness"))
        .arg("logs")
        .current_dir(&root)
        .env_remove("HAARNESS_DATA_DIR")
        .env_remove("HAARNESS_HOME")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("HAARNESS_TELEMETRY", "0")
        .env("HAARNESS_CLOUD_API_URL", "http://127.0.0.1:9")
        .env("RUST_LOG", "warn")
        .output()
        .unwrap();
    let mode = fs::metadata(&server).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    fs::remove_dir_all(root).unwrap();
}

/// A schedule that is written but not registered
/// (`HAARNESS_SCHEDULE_SKIP_INSTALL`) stays in the data root, and the run it
/// defines carries the variable too: started natively by a test, it
/// registers nothing either.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_run_of_an_unregistered_schedule_registers_nothing_either() {
    let root = std::env::temp_dir().join(format!("haarness-schedule-{}", uuid::Uuid::now_v7()));
    for folder in ["home", "workspace", "data"] {
        fs::create_dir_all(root.join(folder)).unwrap();
    }
    let root = root.canonicalize().unwrap();
    // Set at all, even empty, the variable skips registering.
    for (name, skip) in [("fixture", "1"), ("empty", "")] {
        let output = haarness_with(
            &root,
            &[
                "schedule",
                "add",
                "--name",
                name,
                "--prompt",
                "fixture",
                "--model",
                "echo/echo-1",
                "--every",
                "1h",
            ],
            &[("HAARNESS_SCHEDULE_SKIP_INSTALL", skip)],
        );
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let id = stdout.split_whitespace().next().unwrap();
        let definition = if cfg!(target_os = "macos") {
            format!("data/config/scheduler/launchd/dev.haarness.schedule.{id}.plist")
        } else {
            format!("data/config/scheduler/systemd/haarness-schedule-{id}.service")
        };
        let definition = fs::read_to_string(root.join(definition)).unwrap();
        assert!(
            definition.contains("HAARNESS_SCHEDULE_SKIP_INSTALL"),
            "{name}: {definition}"
        );
    }
    for folder in ["home/Library/LaunchAgents", "home/.config/systemd"] {
        assert!(!root.join(folder).exists(), "{folder}");
    }
    fs::remove_dir_all(root).unwrap();
}
