//! Computer use gets more reach for agents, and a pause on the strip ends, only at a person's word. A request from an
//! agent's command is refused, also from a program the command put in the background (`nohup … &` in a subshell, which
//! the system hands to launchd, outside Haarness's process tree), while the command runs and after its own shell has
//! ended; a person's own client is answered, and switching off always works. The same holds for a Haarness run from a
//! file under another name. The server runs the stand-in helper, so nothing touches the screen.
#![cfg(target_os = "macos")]

use haarness_computer::{INSIDE_REFUSAL, INSIDE_RESUME_REFUSAL, terms};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    /// The Haarness program this fixture runs.
    program: PathBuf,
    url: String,
    token: String,
}

impl Fixture {
    fn new() -> Self {
        Self::running(None)
    }

    /// A fixture running Haarness from a file called `renamed` (the built program, linked or copied), or as built.
    fn running(renamed: Option<&str>) -> Self {
        let root = std::env::temp_dir().join(format!("haarness-computer-guard-{}", Uuid::now_v7()));
        let built = PathBuf::from(env!("CARGO_BIN_EXE_haarness"));
        let program = renamed.map_or_else(
            || built.clone(),
            |name| {
                let program = root.join("bin").join(name);
                std::fs::create_dir_all(root.join("bin")).expect("create the program's folder");
                std::fs::hard_link(&built, &program)
                    .or_else(|_| std::fs::copy(&built, &program).map(drop))
                    .expect("put the program under its new name");
                program
            },
        );
        let workspace = root.join("workspace");
        std::fs::create_dir_all(workspace.join(".haarness")).expect("create the project");
        for folder in ["home", "data"] {
            std::fs::create_dir_all(root.join(folder)).expect("create the fixture's folders");
        }
        // Yolo, so the agent's commands run unasked: the guard must hold even there.
        std::fs::write(
            workspace.join(".haarness/config.toml"),
            "[defaults]\nprovider = \"echo\"\nmodel = \"echo-1\"\napproval_mode = \"yolo\"\n",
        )
        .expect("write the project's configuration");
        let mut fixture = Self {
            root,
            workspace,
            program,
            url: String::new(),
            token: String::new(),
        };
        let mut connect = fixture.haarness();
        connect
            .arg("desktop")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = connect.spawn().expect("start the Desktop bridge");
        {
            use std::io::Write as _;
            child
                .stdin
                .take()
                .expect("bridge stdin")
                .write_all(br#"{"action":"connect"}"#)
                .expect("ask the bridge to connect");
        }
        let output = child.wait_with_output().expect("the bridge answers");
        let connection: Value =
            serde_json::from_slice(&output.stdout).expect("the bridge answers JSON");
        connection["url"]
            .as_str()
            .expect("a server address")
            .clone_into(&mut fixture.url);
        connection["token"]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut fixture.token);
        fixture
    }

    fn haarness(&self) -> Command {
        let mut command = Command::new(&self.program);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("HAARNESS_") {
                command.env_remove(name);
            }
        }
        command
            .arg("--workspace")
            .arg(&self.workspace)
            .current_dir(&self.workspace)
            .env("HOME", self.root.join("home"))
            .env("HAARNESS_HOME", self.root.join("home"))
            .env("HAARNESS_DATA_DIR", self.root.join("data"))
            .env("HAARNESS_TEST_PROVIDER", "1")
            .env("HAARNESS_TELEMETRY", "0")
            .env("HAARNESS_CLOUD_API_URL", "http://127.0.0.1:9")
            .env("HAARNESS_SCHEDULE_SKIP_INSTALL", "1")
            .env(
                "HAARNESS_MAC_HELPER",
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../haarness-computer/tests/fake-helper.py"
                ),
            )
            .env("FAKE_HELPER_LOG", self.root.join("helper.log"))
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null());
        command
    }

    /// A request from this test: a person's own client, outside Haarness.
    fn api(&self, path: &str, body: &Value) -> Value {
        let output = Command::new("curl")
            .args([
                "-s",
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "-H",
            ])
            .arg(format!("Authorization: Bearer {}", self.token))
            .arg(format!("{}/v1{path}", self.url))
            .arg("-d")
            .arg(body.to_string())
            .output()
            .expect("run curl");
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
    }

    fn get(&self, path: &str) -> Value {
        let output = Command::new("curl")
            .args(["-s", "-H"])
            .arg(format!("Authorization: Bearer {}", self.token))
            .arg(format!("{}/v1{path}", self.url))
            .output()
            .expect("run curl");
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
    }

    fn status(&self) -> Value {
        self.api("/manage/computer-use", &json!({"action": "get"}))["data"].clone()
    }

    fn chat(&self, title: &str) -> String {
        self.api(
            "/threads",
            &json!({"title": title, "provider": "echo", "model": "echo-1"}),
        )["id"]
            .as_str()
            .expect("a chat id")
            .to_owned()
    }

    fn turn(&self, chat: &str, text: &str) -> String {
        self.api(
            &format!("/threads/{chat}/turns"),
            &json!({"requestId": Uuid::now_v7().to_string(), "text": text}),
        )["id"]
            .as_str()
            .expect("a turn id")
            .to_owned()
    }

    fn turn_status(&self, turn: &str) -> String {
        self.get(&format!("/turns/{turn}"))["status"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// Runs `script` as an agent's `bash` command in `chat`, and waits for that command's turn to end.
    fn agent_runs(&self, chat: &str, script: &str) {
        let turn = self.turn(
            chat,
            &format!("!tool bash {}", json!({ "command": script })),
        );
        until("the agent's command ends", || {
            !matches!(self.turn_status(&turn).as_str(), "running" | "queued" | "")
        });
    }

    /// A POST to the computer use resource, as a shell line writing the answer to `answer`.
    fn post_line(&self, body: &Value, answer: &Path) -> String {
        format!(
            "curl -s -X POST -H 'Authorization: Bearer {}' -H 'Content-Type: application/json' {}/v1/manage/computer-use -d '{}' > '{}'\n",
            self.token,
            self.url,
            body,
            answer.display()
        )
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join(name)).unwrap_or_default()
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
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A shell line that waits up to 30 seconds for `file` to appear: the command lasts while its background program asks.
fn wait_for(file: &Path) -> String {
    format!(
        "for _ in $(seq 300); do [ -e '{}' ] && break; sleep 0.1; done; echo done",
        file.display()
    )
}

/// The app rules in a status, as bundle and rule.
fn rules(status: &Value) -> Vec<(String, String)> {
    status["settings"]["apps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|app| {
            (
                app["bundle"].as_str().unwrap_or_default().to_owned(),
                app["rule"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn only_a_person_widens_computer_use_or_ends_a_pause_even_from_a_program_left_running() {
    let fixture = Fixture::new();
    // The person agrees and turns it on from their own client.
    let accepted = fixture.api(
        "/manage/computer-use",
        &json!({"action": "accept", "version": terms::VERSION}),
    );
    assert_eq!(accepted["data"]["enabled"], true, "{accepted}");
    let allowed = fixture.api(
        "/manage/computer-use",
        &json!({"action": "app", "bundle": "com.apple.TextEdit", "rule": "allow"}),
    );
    let only_text_edit = vec![("com.apple.TextEdit".to_owned(), "allow".to_owned())];
    assert_eq!(rules(&allowed["data"]), only_text_edit, "{allowed}");

    // Chat A's typing is paused on the strip and waits.
    let paused_chat = fixture.chat("Paused");
    let held = fixture.turn(
        &paused_chat,
        &format!(
            "!tool mac_type {}",
            json!({"ref": "e2", "text": "HOLD", "snapshot": false})
        ),
    );
    until("the pause", || fixture.status()["paused"] == true);
    let agent = fixture.chat("Agent");

    // An agent's command asks for more reach and for the pause to end, still inside Haarness's tree.
    let root = &fixture.root;
    let attached = root.join("attached.sh");
    std::fs::write(
        &attached,
        [
            fixture.post_line(
                &json!({"action": "resume"}),
                &root.join("attached-resume.json"),
            ),
            fixture.post_line(
                &json!({"action": "configure", "defaultAct": "allow"}),
                &root.join("attached-configure.json"),
            ),
        ]
        .concat(),
    )
    .unwrap();
    fixture.agent_runs(&agent, &format!("sh '{}'", attached.display()));
    assert!(
        fixture
            .read("attached-resume.json")
            .contains(INSIDE_RESUME_REFUSAL),
        "{}",
        fixture.read("attached-resume.json")
    );
    assert!(
        fixture
            .read("attached-configure.json")
            .contains(INSIDE_REFUSAL),
        "{}",
        fixture.read("attached-configure.json")
    );

    // The same from a program the command puts in the background, which runs under launchd by the time it asks. The
    // command waits for it, so its shell still leads the group: the group's live leader is what tells here. It also
    // tries `haarness mac` without the chat's variable.
    let detached = root.join("detached.sh");
    std::fs::write(
        &detached,
        [
            "sleep 1\n".to_owned(),
            fixture.post_line(&json!({"action": "resume"}), &root.join("left-resume.json")),
            fixture.post_line(
                &json!({"action": "configure", "defaultAct": "allow"}),
                &root.join("left-configure.json"),
            ),
            fixture.post_line(
                &json!({"action": "app", "bundle": "com.apple.Notes", "rule": "allow"}),
                &root.join("left-app.json"),
            ),
            fixture.post_line(
                &json!({"action": "accept", "version": terms::VERSION}),
                &root.join("left-accept.json"),
            ),
            format!(
                "env -u HAARNESS_SESSION_ID '{}' mac allow com.apple.Mail > '{}' 2>&1; echo \"exit $?\" >> '{}'\n",
                env!("CARGO_BIN_EXE_haarness"),
                root.join("left-cli.txt").display(),
                root.join("left-cli.txt").display()
            ),
            format!("touch '{}'\n", root.join("left-done").display()),
        ]
        .concat(),
    )
    .unwrap();
    fixture.agent_runs(
        &agent,
        &format!(
            "( nohup sh '{}' > '{}' 2>&1 & ) ; {}",
            detached.display(),
            root.join("detached.log").display(),
            wait_for(&root.join("left-done"))
        ),
    );
    assert!(
        root.join("left-done").exists(),
        "{}",
        fixture.read("detached.log")
    );
    for (name, refusal) in [
        ("left-resume.json", INSIDE_RESUME_REFUSAL),
        ("left-configure.json", INSIDE_REFUSAL),
        ("left-app.json", INSIDE_REFUSAL),
        ("left-accept.json", INSIDE_REFUSAL),
        ("left-cli.txt", INSIDE_REFUSAL),
    ] {
        assert!(
            fixture.read(name).contains(refusal),
            "{name}: {}",
            fixture.read(name)
        );
    }
    assert!(!fixture.read("left-cli.txt").contains("exit 0"));
    let status = fixture.status();
    assert_eq!(status["paused"], true, "{status}");
    assert_eq!(status["settings"]["defaultAct"], "ask", "{status}");
    assert_eq!(rules(&status), only_text_edit, "{status}");
    assert_eq!(fixture.turn_status(&held), "running");

    // A program still in the command's group after the command's own shell has ended and been reaped, asking before
    // the server stops the group. Nothing in the process table leads that group any more, and the program runs under
    // launchd: only the server's note of the groups it started tells. The program keeps printing, so the server reads
    // on, for two seconds at most, before it stops the group; the program asks once the shell has left the table.
    // A stop that comes first (a very busy machine) leaves an empty answer, and the round is run again.
    let mut refused = (false, false);
    for round in 0..3 {
        if refused == (true, true) {
            break;
        }
        let file = |name: &str| root.join(format!("ended-{round}-{name}"));
        let lingering = root.join(format!("ended-{round}.sh"));
        std::fs::write(
            &lingering,
            [
                "shell=$(ps -o pgid= -p $$ | tr -d ' ')\n".to_owned(),
                "( while :; do echo tick; sleep 0.05; done ) &\nticker=$!\n".to_owned(),
                "while ps -p \"$shell\" > /dev/null 2>&1; do sleep 0.02; done\n".to_owned(),
                format!("touch '{}'\n", file("shell-gone").display()),
                fixture.post_line(&json!({"action": "resume"}), &file("resume.json")),
                fixture.post_line(
                    &json!({"action": "configure", "defaultAct": "allow"}),
                    &file("configure.json"),
                ),
                "kill \"$ticker\"\n".to_owned(),
            ]
            .concat(),
        )
        .unwrap();
        fixture.agent_runs(
            &agent,
            &format!("sh '{}' & echo started", lingering.display()),
        );
        let status = fixture.status();
        assert_eq!(status["paused"], true, "round {round}: {status}");
        assert_eq!(
            status["settings"]["defaultAct"], "ask",
            "round {round}: {status}"
        );
        assert_eq!(fixture.turn_status(&held), "running");
        for (name, refusal, seen) in [
            ("resume", INSIDE_RESUME_REFUSAL, &mut refused.0),
            ("configure", INSIDE_REFUSAL, &mut refused.1),
        ] {
            let answer = file(&format!("{name}.json"));
            let text = std::fs::read_to_string(answer).unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            assert!(text.contains(refusal), "round {round} {name}: {text}");
            assert!(file("shell-gone").exists(), "round {round}");
            *seen = true;
        }
    }
    assert_eq!(
        refused,
        (true, true),
        "the group was stopped before its program could ask, three times"
    );

    // The person's own Resume lets chat A go on.
    let resumed = fixture.api("/manage/computer-use", &json!({"action": "resume"}));
    assert_eq!(resumed["data"]["paused"], false, "{resumed}");
    until("chat A's turn ends", || {
        fixture.turn_status(&held) != "running"
    });

    // Switching off works from anywhere, a program left running included.
    let off = root.join("off.sh");
    std::fs::write(
        &off,
        [
            "sleep 1\n".to_owned(),
            fixture.post_line(
                &json!({"action": "enable", "enabled": false}),
                &root.join("left-off.json"),
            ),
            format!("touch '{}'\n", root.join("off-done").display()),
        ]
        .concat(),
    )
    .unwrap();
    fixture.agent_runs(
        &agent,
        &format!(
            "( nohup sh '{}' > /dev/null 2>&1 & ) ; {}",
            off.display(),
            wait_for(&root.join("off-done"))
        ),
    );
    assert!(root.join("off-done").exists());
    assert_eq!(fixture.status()["enabled"], false);

    // And the person widens as they please.
    let configured = fixture.api(
        "/manage/computer-use",
        &json!({"action": "configure", "defaultAct": "allow"}),
    );
    assert_eq!(
        configured["data"]["settings"]["defaultAct"], "allow",
        "{configured}"
    );
}

/// A Haarness run from a file under another name refuses its agents all the same: the server checking a request knows
/// every program below itself, `haarness mac` knows a server run from the same file as itself, and a copy kept under a
/// longer name (`haarness-test`) is known to any `haarness`. The person's own `haarness mac` from that file still works.
#[test]
fn a_renamed_haarness_still_refuses_its_agents_and_answers_its_person() {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_haarness"));
    for (renamed, from_built) in [("hn", false), ("haarness-test", true)] {
        let fixture = Fixture::running(Some(renamed));
        // The person agrees, then switches off, so that switching on would widen too.
        let accepted = fixture.api(
            "/manage/computer-use",
            &json!({"action": "accept", "version": terms::VERSION}),
        );
        assert_eq!(accepted["data"]["enabled"], true, "{renamed}: {accepted}");
        let off = fixture.api(
            "/manage/computer-use",
            &json!({"action": "enable", "enabled": false}),
        );
        assert_eq!(off["data"]["enabled"], false, "{renamed}: {off}");

        // The agent's command asks the server, runs `haarness mac` without the chat's variable, and leaves a program
        // in the background that switches on.
        let program = if from_built { &built } else { &fixture.program };
        let mac = |arguments: &str, answer: &str| {
            format!(
                "env -u HAARNESS_SESSION_ID '{}' --workspace '{}' mac {arguments} > '{}' 2>&1; echo \"exit $?\" >> '{}'",
                program.display(),
                fixture.workspace.display(),
                fixture.root.join(answer).display(),
                fixture.root.join(answer).display()
            )
        };
        let root = &fixture.root;
        let left = root.join("left.sh");
        std::fs::write(
            &left,
            format!(
                "sleep 1\n{}\ntouch '{}'\n",
                mac("on", "left-cli.txt"),
                root.join("left-done").display()
            ),
        )
        .unwrap();
        let script = root.join("widen.sh");
        std::fs::write(
            &script,
            [
                fixture.post_line(
                    &json!({"action": "app", "bundle": "com.apple.Notes", "rule": "allow"}),
                    &root.join("api.json"),
                ),
                format!("{}\n", mac("allow com.apple.Mail", "cli.txt")),
                format!("( nohup sh '{}' > /dev/null 2>&1 & )\n", left.display()),
                format!("{}\n", wait_for(&root.join("left-done"))),
            ]
            .concat(),
        )
        .unwrap();
        let agent = fixture.chat("Agent");
        fixture.agent_runs(&agent, &format!("sh '{}'", script.display()));
        assert!(root.join("left-done").exists(), "{renamed}");
        for name in ["api.json", "cli.txt", "left-cli.txt"] {
            assert!(
                fixture.read(name).contains(INSIDE_REFUSAL),
                "{renamed} {name}: {}",
                fixture.read(name)
            );
            assert!(!fixture.read(name).contains("exit 0"), "{renamed} {name}");
        }
        let status = fixture.status();
        assert_eq!(status["enabled"], false, "{renamed}: {status}");
        assert_eq!(rules(&status), Vec::new(), "{renamed}: {status}");

        // The person, in a terminal of their own.
        let person = fixture
            .haarness()
            .args(["mac", "allow", "com.apple.Mail"])
            .output()
            .expect("run haarness mac");
        assert!(
            person.status.success(),
            "{renamed}: {}",
            String::from_utf8_lossy(&person.stderr)
        );
        assert_eq!(
            rules(&fixture.status()),
            vec![("com.apple.Mail".to_owned(), "allow".to_owned())],
            "{renamed}"
        );
    }
}
