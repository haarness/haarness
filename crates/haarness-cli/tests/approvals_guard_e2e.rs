//! Only a person lets agents do more without asking. An agent's command (with the chat's variable cleared) cannot
//! start a `--yolo` run, put a chat in a higher approval mode with `--approvals`, raise one through the server, start a
//! turn that approves its own requests, or start a server that approves everything; it can still lower a chat's mode.
//! Nor can it, or a program it leaves running, allow a waiting request, answer a question or open a terminal. The
//! person's own client and terminal do all of that as before, and so do programs the person runs in a terminal the
//! server opened, as in Desktop's terminal panel; a scheduled run's turn still approves its own requests, also when
//! `haarness schedule run` starts it.
#![cfg(target_os = "macos")]

use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

/// What the command line says when it refuses a flag.
const CLI_REFUSAL: &str = "Only you can let agents do more without asking";

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    url: String,
    token: String,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("haarness-approvals-guard-{}", Uuid::now_v7()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(workspace.join(".haarness")).expect("create the project");
        std::fs::create_dir_all(workspace.join("out")).expect("create the answers folder");
        for folder in ["home", "data"] {
            std::fs::create_dir_all(root.join(folder)).expect("create the fixture's folders");
        }
        // Review, with the agent's commands allowed by a rule, so they run unasked and only their effect is judged.
        std::fs::write(
            workspace.join(".haarness/config.toml"),
            "[defaults]\nprovider = \"echo\"\nmodel = \"echo-1\"\napproval_mode = \"review\"\n[permissions]\nbash = \"allow\"\n[lsp]\nenabled = false\n",
        )
        .expect("write the project's configuration");
        let mut fixture = Self {
            root,
            workspace,
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

    /// The Haarness command line, as the person runs it in a terminal of their own.
    fn haarness(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("HAARNESS_") {
                command.env_remove(name);
            }
        }
        command
            .args(self.program_args())
            .current_dir(&self.workspace)
            .env("HOME", self.root.join("home"))
            .env("HAARNESS_HOME", self.root.join("home"))
            .env("HAARNESS_DATA_DIR", self.root.join("data"))
            .env("HAARNESS_TEST_PROVIDER", "1")
            .env("HAARNESS_TELEMETRY", "0")
            .env("HAARNESS_CLOUD_API_URL", "http://127.0.0.1:9")
            .env("HAARNESS_SCHEDULE_SKIP_INSTALL", "1")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null());
        command
    }

    fn program_args(&self) -> Vec<String> {
        vec![
            "--workspace".into(),
            self.workspace.display().to_string(),
            "--trust-project".into(),
        ]
    }

    /// The same command line inside an agent's command, without the chat's variable.
    fn agent_line(&self, arguments: &str, answer: &str) -> String {
        format!(
            "env -u HAARNESS_SESSION_ID '{}' {} {arguments} > out/{answer} 2>&1; echo \"exit $?\" >> out/{answer}\n",
            env!("CARGO_BIN_EXE_haarness"),
            self.program_args()
                .iter()
                .map(|argument| format!("'{argument}'"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    /// A request to the server as a shell line, its answer written to `answer`.
    fn curl_line(&self, method: &str, path: &str, body: &Value, answer: &str) -> String {
        format!(
            "curl -s -X {method} -H 'Authorization: Bearer {}' -H 'Content-Type: application/json' {}/v1{path} -d '{body}' > out/{answer}\n",
            self.token, self.url
        )
    }

    /// A request from this test: a person's own client, outside Haarness.
    fn api(&self, method: &str, path: &str, body: Option<&Value>) -> Value {
        let mut command = Command::new("curl");
        command
            .args([
                "-s",
                "-X",
                method,
                "-H",
                "Content-Type: application/json",
                "-H",
            ])
            .arg(format!("Authorization: Bearer {}", self.token))
            .arg(format!("{}/v1{path}", self.url));
        if let Some(body) = body {
            command.arg("-d").arg(body.to_string());
        }
        let output = command.output().expect("run curl");
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
    }

    fn chat(&self, title: &str) -> String {
        self.api(
            "POST",
            "/threads",
            Some(&json!({"title": title, "provider": "echo", "model": "echo-1"})),
        )["id"]
            .as_str()
            .expect("a chat id")
            .to_owned()
    }

    fn mode(&self, chat: &str) -> String {
        self.api("GET", &format!("/threads/{chat}/approval-mode"), None)["mode"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn set_mode(&self, chat: &str, mode: &str) -> Value {
        self.api(
            "PUT",
            &format!("/threads/{chat}/approval-mode"),
            Some(&json!({ "mode": mode })),
        )
    }

    /// Runs `script` as an agent's `bash` command in `chat`, and waits for that command's turn to end.
    fn agent_runs(&self, chat: &str, script: &str) {
        let turn = self.api(
            "POST",
            &format!("/threads/{chat}/turns"),
            Some(&json!({
                "requestId": Uuid::now_v7().to_string(),
                "text": format!("!tool bash {}", json!({ "command": script })),
            })),
        )["id"]
            .as_str()
            .expect("a turn id")
            .to_owned();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let status = self.api("GET", &format!("/turns/{turn}"), None)["status"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if !matches!(status.as_str(), "running" | "queued" | "") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the agent's command"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Starts a turn in `chat` and returns its id, without waiting for it.
    fn start(&self, chat: &str, text: &str) -> String {
        self.api(
            "POST",
            &format!("/threads/{chat}/turns"),
            Some(&json!({"requestId": Uuid::now_v7().to_string(), "text": text})),
        )["id"]
            .as_str()
            .expect("a turn id")
            .to_owned()
    }

    fn turn_status(&self, turn: &str) -> String {
        self.api("GET", &format!("/turns/{turn}"), None)["status"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// The ids of what waits for a person in `chat`: its requests, or its questions.
    fn waiting(&self, kind: &str, chat: &str) -> Vec<String> {
        let key = if kind == "permissions" {
            "permissionId"
        } else {
            "questionId"
        };
        self.api("GET", &format!("/{kind}"), None)
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["threadId"] == chat)
            .filter_map(|item| item[key].as_str().map(str::to_owned))
            .collect()
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.workspace.join("out").join(name)).unwrap_or_default()
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

#[test]
fn only_a_person_raises_approvals_and_a_scheduled_run_still_approves_its_own() {
    let fixture = Fixture::new();
    let agent = fixture.chat("Agent");
    let other = fixture.chat("Other");
    let lowered = fixture.chat("Lowered");
    for chat in [&agent, &other] {
        assert_eq!(fixture.mode(chat), "review");
    }
    // The person puts one chat in Yolo from their own client.
    assert_eq!(fixture.set_mode(&lowered, "yolo")["mode"], "yolo");

    let script = [
        fixture.agent_line("--yolo run 'hello'", "yolo.txt"),
        fixture.agent_line(&format!("--approvals yolo -s {other} run 'hello'"), "approvals.txt"),
        fixture.agent_line(&format!("--auto -s {other} run 'hello'"), "auto.txt"),
        // A server that approves everything: refused at once, else stopped after five seconds.
        format!(
            "( env -u HAARNESS_SESSION_ID '{}' {} --yolo serve --background-child --port 0 > out/serve.txt 2>&1 & server=$!; for _ in $(seq 50); do kill -0 $server 2>/dev/null || break; sleep 0.1; done; kill $server 2>/dev/null; wait $server; echo \"exit $?\" >> out/serve.txt )\n",
            env!("CARGO_BIN_EXE_haarness"),
            fixture.program_args().join(" ")
        ),
        fixture.curl_line(
            "PUT",
            &format!("/threads/{other}/approval-mode"),
            &json!({"mode": "yolo"}),
            "raise.json",
        ),
        fixture.curl_line(
            "PUT",
            &format!("/threads/{agent}/approval-mode"),
            &json!({"mode": "auto"}),
            "raise-own.json",
        ),
        fixture.curl_line(
            "POST",
            &format!("/threads/{other}/turns"),
            &json!({"requestId": Uuid::now_v7().to_string(), "text": "hello", "autoApprove": true}),
            "turn.json",
        ),
        // Lowering is never refused.
        fixture.curl_line(
            "PUT",
            &format!("/threads/{lowered}/approval-mode"),
            &json!({"mode": "review"}),
            "lower.json",
        ),
    ]
    .concat();
    std::fs::write(fixture.workspace.join("out/widen.sh"), script).unwrap();
    fixture.agent_runs(&agent, "sh out/widen.sh");

    for name in ["yolo.txt", "approvals.txt", "auto.txt", "serve.txt"] {
        let answer = fixture.read(name);
        assert!(answer.contains(CLI_REFUSAL), "{name}: {answer}");
        assert!(!answer.contains("exit 0"), "{name}: {answer}");
    }
    for name in ["raise.json", "raise-own.json"] {
        let answer = fixture.read(name);
        assert!(
            answer.contains(haarness_core::APPROVALS_RAISE_REFUSAL),
            "{name}: {answer}"
        );
    }
    let turn = fixture.read("turn.json");
    assert!(turn.contains(haarness_core::AUTO_APPROVE_REFUSAL), "{turn}");
    let lower: Value = serde_json::from_str(&fixture.read("lower.json")).unwrap_or(Value::Null);
    assert_eq!(lower["mode"], "review", "{lower}");
    assert_eq!(fixture.mode(&other), "review");
    assert_eq!(fixture.mode(&agent), "review");
    assert_eq!(fixture.mode(&lowered), "review");

    // The person raises them from their own client and terminal.
    assert_eq!(fixture.set_mode(&agent, "auto")["mode"], "auto");
    let run = fixture
        .haarness()
        .args(["--approvals", "yolo", "-s", &other, "run", "hello"])
        .output()
        .expect("run haarness");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(fixture.mode(&other), "yolo");

    // A scheduled run started from the person's terminal: its runner is a program Haarness started, and its turn
    // approves its own requests all the same.
    let added = fixture
        .haarness()
        .args([
            "schedule",
            "add",
            "--name",
            "nightly",
            "--every",
            "1h",
            "--disabled",
            "--model",
            "echo/echo-1",
            "--prompt",
            "scheduled hello",
        ])
        .output()
        .expect("add a schedule");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let ran = fixture
        .haarness()
        .args(["schedule", "run", "nightly", "--json"])
        .output()
        .expect("run the schedule");
    let record: Value = serde_json::from_slice(&ran.stdout).unwrap_or(Value::Null);
    assert_eq!(
        record["status"],
        "success",
        "{record} {}",
        String::from_utf8_lossy(&ran.stderr)
    );
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn only_a_person_answers_an_agent_and_what_they_run_in_a_haarness_terminal_is_theirs() {
    let fixture = Fixture::new();
    let outside = fixture.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    // A request (a command outside the project) and a question wait for a person.
    let asking = fixture.chat("Asking");
    let asked = fixture.start(
        &asking,
        &format!(
            "!tool bash {}",
            json!({"command": format!("touch '{}'", outside.join("asked").display())})
        ),
    );
    let questioning = fixture.chat("Questioning");
    let questioned = fixture.start(
        &questioning,
        &format!(
            "!tool question {}",
            json!({"prompt": "Which one?", "choices": ["left", "right"]})
        ),
    );
    until("the request and the question", || {
        !fixture.waiting("permissions", &asking).is_empty()
            && !fixture.waiting("questions", &questioning).is_empty()
    });
    let permission = fixture.waiting("permissions", &asking).remove(0);
    let question = fixture.waiting("questions", &questioning).remove(0);

    // An agent's command answers them itself, and so does a program it leaves running; it also opens a terminal to
    // answer from there.
    let answers = [
        fixture.curl_line(
            "POST",
            &format!("/permissions/{permission}"),
            &json!({"decision": "once"}),
            "allow.json",
        ),
        fixture.curl_line(
            "POST",
            &format!("/questions/{question}"),
            &json!({"answer": "left"}),
            "question.json",
        ),
        fixture.curl_line(
            "POST",
            "/ptys",
            &json!({"command": "/bin/sh", "args": ["-c", "touch out/agent-terminal"]}),
            "terminal.json",
        ),
    ]
    .concat();
    let left = [
        "sleep 1\n".to_owned(),
        fixture.curl_line(
            "POST",
            &format!("/permissions/{permission}"),
            &json!({"decision": "always"}),
            "left-allow.json",
        ),
        "touch out/left-done\n".to_owned(),
    ]
    .concat();
    std::fs::write(fixture.workspace.join("out/answers.sh"), answers).unwrap();
    std::fs::write(fixture.workspace.join("out/left.sh"), left).unwrap();
    let agent = fixture.chat("Agent");
    fixture.agent_runs(
        &agent,
        "sh out/answers.sh; ( nohup sh out/left.sh > /dev/null 2>&1 & ) ; for _ in $(seq 300); do [ -e out/left-done ] && break; sleep 0.1; done",
    );
    for (name, refusal) in [
        ("allow.json", haarness_core::PERMISSION_ANSWER_REFUSAL),
        ("left-allow.json", haarness_core::PERMISSION_ANSWER_REFUSAL),
        ("question.json", haarness_core::QUESTION_ANSWER_REFUSAL),
        ("terminal.json", haarness_core::TERMINAL_REFUSAL),
    ] {
        let answer = fixture.read(name);
        assert!(answer.contains(refusal), "{name}: {answer}");
    }
    assert_eq!(
        fixture.waiting("permissions", &asking),
        std::slice::from_ref(&permission)
    );
    assert_eq!(
        fixture.waiting("questions", &questioning),
        std::slice::from_ref(&question)
    );
    assert_eq!(fixture.turn_status(&asked), "running");
    assert!(!outside.join("asked").exists());
    std::thread::sleep(Duration::from_millis(500));
    assert!(!fixture.workspace.join("out/agent-terminal").exists());

    // The person opens a terminal in the server, as Desktop's terminal panel does, and from there allows the request,
    // answers the question and puts another chat in Yolo.
    let other = fixture.chat("Other");
    let typed = [
        "ps -o stat= -p $$ > out/terminal-state.txt\n".to_owned(),
        fixture.curl_line(
            "POST",
            &format!("/permissions/{permission}"),
            &json!({"decision": "once"}),
            "person-allow.json",
        ),
        fixture.curl_line(
            "POST",
            &format!("/questions/{question}"),
            &json!({"answer": "right"}),
            "person-question.json",
        ),
        fixture.agent_line(
            &format!("--approvals yolo -s {other} run 'hello'"),
            "person-run.txt",
        ),
        "touch out/person-done\n".to_owned(),
    ]
    .concat();
    std::fs::write(fixture.workspace.join("out/person.sh"), typed).unwrap();
    let terminal = fixture.api(
        "POST",
        "/ptys",
        Some(&json!({
            "command": "/bin/sh",
            "args": [fixture.workspace.join("out/person.sh").display().to_string()],
            "cwd": fixture.workspace.display().to_string(),
        })),
    );
    assert!(terminal["id"].is_string(), "{terminal}");
    until("the person's terminal", || {
        fixture.workspace.join("out/person-done").exists()
    });
    assert!(
        fixture.read("terminal-state.txt").contains('s'),
        "the terminal's shell leads its session: {}",
        fixture.read("terminal-state.txt")
    );
    for name in ["person-allow.json", "person-question.json"] {
        assert_eq!(fixture.read(name), "", "{name}");
    }
    let run = fixture.read("person-run.txt");
    assert!(run.contains("exit 0"), "{run}");
    assert_eq!(fixture.mode(&other), "yolo");
    until("the answered turns", || {
        fixture.turn_status(&asked) == "completed"
            && fixture.turn_status(&questioned) == "completed"
    });
    assert!(outside.join("asked").exists());
}
