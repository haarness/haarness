//! `--yolo` belongs to the command it was given to. The folder's server
//! outlives that command and serves every chat in the folder, so a client's
//! `--yolo` must never turn it into a server that approves for everyone.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    data: PathBuf,
    home: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("haarness-yolo-{}", Uuid::now_v7()));
        let fixture = Self {
            workspace: root.join("workspace"),
            data: root.join("data"),
            home: root.join("home"),
            outside: root.join("outside"),
            root,
        };
        for directory in [
            &fixture.workspace,
            &fixture.data,
            &fixture.home,
            &fixture.outside,
        ] {
            std::fs::create_dir_all(directory).expect("create yolo fixture directory");
        }
        fixture
    }

    fn haarness(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
        command
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--data-dir")
            .arg(&self.data)
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
            .env("HAARNESS_HOME", &self.home)
            .env("HAARNESS_TEST_PROVIDER", "1")
            .env("SSH_TTY", "fixture")
            .env("RUST_LOG", "warn")
            .env_remove("ANTHROPIC_BASE_URL")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .stdin(Stdio::null());
        command
    }

    /// A run whose agent writes outside the project: a request that asks by
    /// default. Nobody is at this terminal, so an unapproved run stops.
    fn run_writing_outside(&self, yolo: bool, name: &str) -> Output {
        let target = self.outside.join(name);
        let directive = format!(
            "!tool write {}",
            serde_json::json!({"path": target.display().to_string(), "content": "x"})
        );
        let mut command = self.haarness();
        if yolo {
            command.arg("--yolo");
        }
        command
            .args(["run", "--provider", "echo", "-m", "echo-1", &directive])
            .output()
            .expect("launch haarness run")
    }

    fn wrote(&self, name: &str) -> bool {
        self.outside.join(name).exists()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self
            .haarness()
            .args(["serve", "--stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_stopped_for_approval(output: &Output, fixture: &Fixture, name: &str) {
    assert!(
        !output.status.success(),
        "a plain run must not be approved: {}",
        stderr(output)
    );
    assert!(
        stderr(output).contains("Stopped: the agent asked to reach outside the project"),
        "{}",
        stderr(output)
    );
    assert!(!fixture.wrote(name), "{name} was written without approval");
}

fn server_command_line(data: &Path) -> Option<String> {
    let listing = Command::new("ps").args(["-axo", "command"]).output().ok()?;
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .find(|line| line.contains(&*data.to_string_lossy()) && line.contains(" serve "))
        .map(str::to_owned)
}

#[test]
fn a_yolo_run_approves_only_itself_and_leaves_no_yolo_server_behind() {
    let fixture = Fixture::new();

    // No server yet: the run starts one, and approves its own request.
    let first = fixture.run_writing_outside(true, "first.txt");
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(fixture.wrote("first.txt"));
    if cfg!(unix)
        && let Some(server) = server_command_line(&fixture.data)
    {
        assert!(!server.contains("--yolo"), "{server}");
    }

    // The server it left behind asks like any other, and learned no rule.
    let second = fixture.run_writing_outside(false, "second.txt");
    assert_stopped_for_approval(&second, &fixture, "second.txt");

    // A running, ordinary server: --yolo approves this run only.
    let third = fixture.run_writing_outside(true, "third.txt");
    assert!(third.status.success(), "{}", stderr(&third));
    assert!(fixture.wrote("third.txt"));
    let fourth = fixture.run_writing_outside(false, "fourth.txt");
    assert_stopped_for_approval(&fourth, &fixture, "fourth.txt");
}

#[test]
fn a_detached_workflow_cannot_take_yolo_along() {
    let fixture = Fixture::new();
    let output = fixture
        .haarness()
        .args(["--yolo", "workflow", "run", "audit", "--detach"])
        .output()
        .expect("launch haarness workflow run");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("leave out --detach"),
        "{}",
        stderr(&output)
    );
}
