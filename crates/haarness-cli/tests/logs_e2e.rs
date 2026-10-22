//! `haarness logs` belongs to the data folder, not to a project: Desktop runs
//! it from the home folder and the TUI from wherever it started, so it must
//! never register the folder it runs in as a workspace (nor migrate a
//! pre-workspace database into it), and it must work from a folder that has
//! been deleted.
use std::{
    path::{Path, PathBuf},
    process::Command,
};

use uuid::Uuid;

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn haarness(data: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_haarness"))
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", cwd)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run haarness")
}

#[test]
fn logs_commands_register_no_workspace_and_run_from_a_deleted_folder() {
    let root =
        TestDirectory(std::env::temp_dir().join(format!("haarness-logs-e2e-{}", Uuid::now_v7())));
    let data = root.0.join("data");
    let home = root.0.join("home");
    std::fs::create_dir_all(data.join("data/logs")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        data.join("data/logs/haarness.log.2026-10-21"),
        "2026-10-21T08:00:00.000Z 1 serve  INFO haarness_core: fixture line\n",
    )
    .unwrap();
    // A pre-workspace database a registration would move into its first workspace.
    std::fs::write(data.join("data/haarness.db"), b"legacy").unwrap();
    for args in [
        &["logs"][..],
        &["logs", "--tail", "5"],
        &["logs", "level", "--json"],
        &["logs", "level", "debug", "--for", "5m", "--json"],
        &["logs", "level", "stop", "--json"],
        &["logs", "export", "--output", "out/logs.zip", "--json"],
    ] {
        let output = haarness(&data, &home, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(home.join("out/logs.zip").is_file());
    assert!(
        !data.join("data/workspaces.json").exists(),
        "a logs command registered a workspace"
    );
    assert!(!data.join("data/workspaces").exists());
    assert_eq!(
        std::fs::read(data.join("data/haarness.db")).unwrap(),
        b"legacy"
    );
    // A TUI whose folder was deleted still reads its logs: the shell enters
    // the folder, removes it, and runs `haarness` from inside it.
    let gone = root.0.join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(r#"cd "$1" && rmdir "$1" && exec "$2" --data-dir "$3" logs level --json"#)
        .arg("sh")
        .arg(&gone)
        .arg(env!("CARGO_BIN_EXE_haarness"))
        .arg(&data)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin");
    let output = command.output().expect("run haarness");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["level"], serde_json::Value::Null);
}
