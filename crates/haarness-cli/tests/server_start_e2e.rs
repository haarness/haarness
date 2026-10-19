//! A workspace server that cannot start says why, and `serve` starts where
//! it can. The detached server's stderr goes nowhere, so its launcher
//! (`serve --daemon`, and the TUI's and Desktop's own launches) once
//! reported only "exited during startup (exit status: 1)".

use std::process::{Command, Stdio};

use uuid::Uuid;

#[test]
fn a_server_launched_on_a_port_in_use_says_the_port_is_in_use() {
    let root = std::env::temp_dir().join(format!("haarness-server-start-{}", Uuid::now_v7()));
    let workspace = root.join("workspace");
    let data = root.join("data");
    let home = root.join("home");
    for directory in [&workspace, &data, &home] {
        std::fs::create_dir_all(directory).expect("create fixture directory");
    }
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy a port");
    let port = occupied.local_addr().expect("occupied port").port();
    let output = Command::new(env!("CARGO_BIN_EXE_haarness"))
        .arg("--workspace")
        .arg(&workspace)
        .arg("--data-dir")
        .arg(&data)
        .args(["serve", "--daemon", "--port", &port.to_string()])
        .current_dir(&workspace)
        .env("HOME", &home)
        .env("HAARNESS_HOME", &home)
        .env("HAARNESS_TEST_PROVIDER", "1")
        .env_remove("HAARNESS_SERVER_TOKEN")
        .env_remove("HAARNESS_SERVER_PASSWORD")
        .stdin(Stdio::null())
        .output()
        .expect("launch haarness serve --daemon");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("exited during startup"), "{stderr}");
    assert!(
        stderr.contains(&format!("Port {port} on 127.0.0.1 is already in use")),
        "{stderr}"
    );
    // The server's own log says it too, at ERROR.
    let logs = std::fs::read_dir(data.join("data/logs"))
        .expect("logs folder")
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("haarness.log")
        })
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
        .collect::<String>();
    assert!(
        logs.lines().any(|line| line.contains(" ERROR ")
            && line.contains("command failed")
            && line.contains("already in use")),
        "{logs}"
    );
    drop(occupied);
    let _ = std::fs::remove_dir_all(root);
}

/// Runs its closure when dropped, a failed assertion included.
struct Cleanup<F: FnMut()>(F);
impl<F: FnMut()> Drop for Cleanup<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

/// The folder's server, as its runtime record names it.
fn recorded_server(data: &std::path::Path) -> serde_json::Value {
    let workspaces = data.join("cache/workspaces");
    let record = std::fs::read_dir(&workspaces)
        .expect("workspace cache")
        .flatten()
        .map(|entry| entry.path().join("runtime.json"))
        .find(|path| path.exists())
        .expect("the folder's runtime.json");
    serde_json::from_slice(&std::fs::read(record).expect("read runtime.json"))
        .expect("runtime JSON")
}

/// `haarness serve` in a folder whose server already runs (here the one a
/// `haarness run` started in the background, as the TUI and Desktop do) used
/// to stop with "is already running". It now listens beside that server and
/// serves the same chats through it, on a free port when 4096 is taken, and
/// stopping it leaves that server and its runtime record alone.
#[test]
fn a_second_serve_listens_beside_the_running_server() {
    let root = std::env::temp_dir().join(format!("haarness-serve-twice-{}", Uuid::now_v7()));
    let workspace = root.join("workspace");
    let data = root.join("data");
    let home = root.join("home");
    for directory in [&workspace, &data, &home] {
        std::fs::create_dir_all(directory).expect("create fixture directory");
    }
    let haarness = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
        command
            .arg("--workspace")
            .arg(&workspace)
            .arg("--data-dir")
            .arg(&data)
            .current_dir(&workspace)
            .env("HOME", &home)
            .env("HAARNESS_HOME", &home)
            .env("HAARNESS_TEST_PROVIDER", "1")
            .env("RUST_LOG", "warn")
            .env_remove("HAARNESS_SERVER_TOKEN")
            .env_remove("HAARNESS_SERVER_PASSWORD")
            .stdin(Stdio::null());
        command
    };
    let stop = Cleanup(|| {
        let _ = haarness()
            .args(["serve", "--stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });

    let run = haarness()
        .args(["run", "--provider", "echo", "-m", "echo-1", "hello"])
        .output()
        .expect("launch haarness run");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let daemon = recorded_server(&data);
    let daemon_url = daemon["url"].as_str().expect("daemon url").to_owned();
    let daemon_token = daemon["token"].as_str().expect("daemon token").to_owned();

    // The default port is taken, by this test or by whatever already has it.
    let _default_port = std::net::TcpListener::bind("127.0.0.1:4096");
    let mut front = haarness()
        .arg("serve")
        .env("HAARNESS_SERVER_TOKEN", "front-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch the second haarness serve");
    let stdout = front.stdout.take().expect("serve stdout");
    let (sender, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            let _ = sender.send(line);
        }
    });
    let url = loop {
        let Ok(line) = lines.recv_timeout(std::time::Duration::from_secs(30)) else {
            let _ = front.kill();
            let output = front.wait_with_output().expect("serve output");
            panic!(
                "the second serve did not listen: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        if let Some(url) = line.strip_prefix("Haarness server listening on ") {
            break url.trim().to_owned();
        }
    };
    assert!(!url.ends_with(":4096"), "{url}");

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let client = reqwest::Client::new();
        let get = |url: String, token: &str| client.get(url).bearer_auth(token).send();
        // The same server: its pid, its folder, its chats.
        let info: serde_json::Value = get(format!("{url}/v1/info"), "front-token")
            .await
            .expect("front info")
            .error_for_status()
            .expect("front admits its own token")
            .json()
            .await
            .expect("info JSON");
        assert_eq!(info["pid"], daemon["pid"]);
        let through_front: serde_json::Value = get(format!("{url}/v1/threads"), "front-token")
            .await
            .expect("threads through the front")
            .json()
            .await
            .expect("threads JSON");
        let direct: serde_json::Value = get(format!("{daemon_url}/v1/threads"), &daemon_token)
            .await
            .expect("threads from the server")
            .json()
            .await
            .expect("threads JSON");
        assert!(
            through_front
                .as_array()
                .is_some_and(|chats| !chats.is_empty()),
            "{through_front}"
        );
        assert_eq!(through_front, direct);
        // The front's credential is its own.
        let refused = get(format!("{url}/v1/info"), &daemon_token)
            .await
            .expect("front refusal");
        assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
    });
    // Clients still find the server itself.
    assert_eq!(recorded_server(&data), daemon);

    // A port asked for by name must be free, server or not.
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy a port");
    let port = occupied.local_addr().expect("occupied port").port();
    let busy = haarness()
        .args(["serve", "--port", &port.to_string()])
        .output()
        .expect("serve on a busy port");
    let stderr = String::from_utf8_lossy(&busy.stderr);
    assert!(!busy.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("Port {port} on 127.0.0.1 is already in use")),
        "{stderr}"
    );
    drop(occupied);

    // --yolo cannot reach a server that is already running.
    let yolo = haarness()
        .args(["--yolo", "serve", "--port", "0"])
        .output()
        .expect("serve --yolo");
    let stderr = String::from_utf8_lossy(&yolo.stderr);
    assert!(!yolo.status.success(), "{stderr}");
    assert!(stderr.contains("--yolo cannot change"), "{stderr}");

    // Shutting the front down closes only the front.
    runtime.block_on(async {
        let accepted = reqwest::Client::new()
            .post(format!("{url}/v1/shutdown"))
            .bearer_auth("front-token")
            .send()
            .await
            .expect("front shutdown");
        assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = front.try_wait().expect("front status") {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the front did not stop"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut front.stderr.take().expect("serve stderr"), &mut stderr)
        .expect("read serve stderr");
    assert!(status.success(), "{stderr}");
    assert!(
        stderr.contains("port 4096 on 127.0.0.1 is in use, so this server took a free port"),
        "{stderr}"
    );
    assert!(stderr.contains("this address serves it too"), "{stderr}");
    runtime.block_on(async {
        let info = reqwest::Client::new()
            .get(format!("{daemon_url}/v1/info"))
            .bearer_auth(&daemon_token)
            .send()
            .await
            .expect("the server still answers");
        assert!(info.status().is_success());
    });
    assert_eq!(recorded_server(&data), daemon);
    drop(stop);
    let _ = std::fs::remove_dir_all(root);
}
