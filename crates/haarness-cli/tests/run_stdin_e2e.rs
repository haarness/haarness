//! `haarness run "prompt"` started by a program that leaves stdin open and
//! never writes to it (a script's subprocess, an editor's task runner) runs
//! its prompt rather than waiting for that stdin to end.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use uuid::Uuid;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn haarness(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
        command
            .arg("--workspace")
            .arg(self.root.join("workspace"))
            .arg("--data-dir")
            .arg(self.root.join("data"))
            .current_dir(self.root.join("workspace"))
            .env("HOME", self.root.join("home"))
            .env("HAARNESS_HOME", self.root.join("home"))
            .env("HAARNESS_TEST_PROVIDER", "1")
            .env("RUST_LOG", "warn");
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self
            .haarness()
            .args(["serve", "--stop"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_prompted_run_does_not_wait_on_an_idle_stdin() {
    let fixture = Fixture {
        root: std::env::temp_dir().join(format!("haarness-run-stdin-{}", Uuid::now_v7())),
    };
    for folder in ["workspace", "data", "home"] {
        std::fs::create_dir_all(fixture.root.join(folder)).unwrap();
    }
    let mut child = fixture
        .haarness()
        .args(["run", "--provider", "echo", "-m", "echo-1", "hello"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch haarness run");
    // Held open and never written to until the run is over.
    let stdin = child.stdin.take();
    let deadline = Instant::now() + Duration::from_secs(120);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the run waited for its stdin to end");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("hello"), "{stdout}");
}
