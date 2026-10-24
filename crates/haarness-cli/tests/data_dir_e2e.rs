//! `--data-dir` holds everything a run writes, the server's own files too:
//! a test or an isolated install that names one never writes into the app
//! data folder of the person's home.
use std::{fs, path::Path, process::Command};

fn haarness(root: &Path, args: &[&str]) -> std::process::Output {
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
