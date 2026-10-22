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

/// Whether process `pid` is alive.
#[cfg(unix)]
fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// `command` at a terminal of its own: `script` runs it on a new pseudo
/// terminal, which closes, hanging the command up, when `script` dies. What
/// the terminal shows is written to `screen`.
#[cfg(unix)]
fn at_terminal(command: &Command, screen: &Path) -> Command {
    let mut words = vec![command.get_program().to_owned()];
    words.extend(command.get_args().map(ToOwned::to_owned));
    let mut wrapped = Command::new("script");
    if cfg!(target_os = "macos") {
        wrapped.arg("-q").arg(screen).args(&words);
    } else {
        let line = words
            .iter()
            .map(|word| format!("'{}'", word.to_string_lossy().replace('\'', r"'\''")))
            .collect::<Vec<_>>()
            .join(" ");
        wrapped.args(["-q", "-e", "-c", &line]).arg(screen);
    }
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => wrapped.env(name, value),
            None => wrapped.env_remove(name),
        };
    }
    if let Some(directory) = command.get_current_dir() {
        wrapped.current_dir(directory);
    }
    wrapped
}

/// How a terminal's end reaches a `--yolo` run started in it.
#[cfg(unix)]
#[derive(Clone, Copy)]
enum Hangup {
    /// The window closes over an interactive zsh, which sends its job a
    /// second SIGHUP right after the terminal's own.
    ZshWindowCloses,
    /// SIGHUP again and again straight to the run, whose exit status
    /// `script` passes on.
    Twice,
}

#[cfg(unix)]
#[test]
fn closing_the_terminal_of_a_yolo_run_stops_its_turn_as_ctrl_c_does() {
    if Command::new("script").arg("-V").output().is_err() && !Path::new("/usr/bin/script").exists()
    {
        eprintln!("skipping: no `script` to give the run a terminal");
        return;
    }
    hang_up_a_run(Hangup::Twice);
    if Path::new("/bin/zsh").exists() {
        hang_up_a_run(Hangup::ZshWindowCloses);
    } else {
        eprintln!("skipping the zsh case: no /bin/zsh");
    }
}

#[cfg(unix)]
fn hang_up_a_run(hangup: Hangup) {
    use std::time::{Duration, Instant};
    let fixture = Fixture::new();
    // The folder's server is already running, apart from the terminal, as it
    // is when Desktop or a terminal UI started it.
    let warm = fixture
        .haarness()
        .args(["run", "--provider", "echo", "-m", "echo-1", "hello"])
        .output()
        .expect("start the folder's server");
    assert!(warm.status.success(), "{}", stderr(&warm));
    // The shell the turn runs says who it is, then waits.
    let pid_file = fixture.workspace.join("shell.pid");
    let directive = format!(
        "!tool bash {}",
        serde_json::json!({"command": "echo $$ > shell.pid; sleep 45"})
    );
    let mut run = fixture.haarness();
    run.args([
        "--yolo",
        "run",
        "--provider",
        "echo",
        "-m",
        "echo-1",
        &directive,
    ]);
    if let Hangup::ZshWindowCloses = hangup {
        // `; :` keeps zsh from replacing itself with the run.
        let mut words = vec![run.get_program().to_owned()];
        words.extend(run.get_args().map(ToOwned::to_owned));
        let line = words
            .iter()
            .map(|word| format!("'{}'", word.to_string_lossy().replace('\'', r"'\''")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut zsh = fixture.haarness();
        let envs: Vec<_> = zsh
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned)))
            .collect();
        zsh = Command::new("/bin/zsh");
        for (name, value) in envs {
            match value {
                Some(value) => zsh.env(name, value),
                None => zsh.env_remove(name),
            };
        }
        zsh.args(["-f", "-i", "-c", &format!("{line}; :")])
            .current_dir(&fixture.workspace);
        run = zsh;
    }
    let screen = fixture.root.join("screen.txt");
    let mut terminal = at_terminal(&run, &screen)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("launch the run at a terminal");
    let wait = |until: &dyn Fn() -> bool, limit: Duration| {
        let started = Instant::now();
        while started.elapsed() < limit {
            if until() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    };
    assert!(
        wait(
            &|| std::fs::read_to_string(&pid_file).is_ok_and(|pid| !pid.trim().is_empty()),
            Duration::from_secs(90)
        ),
        "the run's command started"
    );
    let shell = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_owned();
    assert!(alive(&shell));
    match hangup {
        Hangup::ZshWindowCloses => {
            // The terminal window closes: its processes get SIGHUP.
            terminal.kill().expect("close the terminal");
            let _ = terminal.wait();
        }
        Hangup::Twice => {
            use nix::{sys::signal, unistd::Pid};
            let children = Command::new("pgrep")
                .args(["-P", &terminal.id().to_string()])
                .output()
                .expect("list the terminal's processes");
            let run: i32 = String::from_utf8_lossy(&children.stdout)
                .split_whitespace()
                .next()
                .and_then(|pid| pid.parse().ok())
                .expect("the run is the terminal's process");
            // The first starts the stop; the later ones, a fraction of a
            // millisecond apart as a closing shell's are, come while it is on
            // its way to the server.
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(20) {
                if signal::kill(Pid::from_raw(run), signal::Signal::SIGHUP).is_err() {
                    break;
                }
                let sent = Instant::now();
                while sent.elapsed() < Duration::from_micros(50) {
                    std::hint::spin_loop();
                }
            }
            let status = terminal.wait().expect("the run exits");
            assert_eq!(status.code(), Some(129), "a hung-up run exits as one");
            // The later hang-ups did not cut the stop short.
            let shown = std::fs::read_to_string(&screen).unwrap_or_default();
            assert!(
                shown.contains("Stopped. The chat keeps what was done so far."),
                "{shown}"
            );
        }
    }
    assert!(
        wait(&|| !alive(&shell), Duration::from_secs(20)),
        "the run's turn stops with its terminal instead of going on in the server"
    );
}
