//! Real CLI archive lifecycle, real authenticated HTTP catalog, and restored runtime discovery.
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::Duration,
};

struct Fixture {
    root: PathBuf,
    server: Option<Child>,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("haarness-profiles-e2e-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        for name in ["source", "target", "cloud"] {
            fs::create_dir_all(root.join(name).join("workspace")).unwrap();
        }
        fs::create_dir_all(root.join("home")).unwrap();
        Self { root, server: None }
    }
    fn command(&self, name: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
        // `--data-dir` holds Haarness's own folders. The home holds the rest
        // a run reads (the Cloud sign-in, `~/.claude`, global skills), so
        // the person's own never reach the test, and usage statistics stay
        // off with Cloud out of reach.
        let home = self.root.join("home");
        command
            .args([
                "--data-dir",
                self.root.join(name).to_str().unwrap(),
                "--workspace",
                self.root.join(name).join("workspace").to_str().unwrap(),
            ])
            .env_remove("HAARNESS_PROFILE_URL")
            .env_remove("HAARNESS_PROFILE_TOKEN")
            .env_remove("HAARNESS_DATA_DIR")
            .env_remove("HAARNESS_CLOUD_CONFIG_DIR")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("HAARNESS_HOME", self.root.join("haarness-home"))
            .env("HAARNESS_CLOUD_API_URL", "http://127.0.0.1:9")
            .env("HAARNESS_TELEMETRY", "0")
            .env("HAARNESS_TEST_PROVIDER", "1")
            .env("RUST_LOG", "warn");
        command
    }
    fn output(&self, name: &str, args: &[&str]) -> Output {
        self.command(name).args(args).output().unwrap()
    }
    fn ok(&self, name: &str, args: &[&str]) -> Value {
        let output = self.output(name, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stdout)))
    }
    fn put(&self, name: &str, path: &str, content: &str) {
        let path = self.root.join(name).join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    fn archive(&self) -> PathBuf {
        self.root.join("portable.zip")
    }
    fn seed(&self) {
        self.put(
            "source",
            "external-plugin/.claude-plugin/plugin.json",
            "{\"name\":\"portable-plugin\",\"version\":\"1.0.0\"}",
        );
        self.put("source", "external-plugin/commands/check.md", "---\ndescription: Portable plugin check\n---\nReport that the portable plugin loaded.\n");
        self.put("source", "data/plugins/installed.json", &serde_json::json!({"portable-plugin":{
            "path":self.root.join("source/external-plugin"), "enabled":true, "marketplace":"local"
        }}).to_string());
        self.put(
            "source",
            "config/agents/portable-review.md",
            "---\ndescription: Portable review agent\nmode: all\n---\nReview changes carefully.\n",
        );
        self.put(
            "source",
            "config/haarness.json",
            "{\"model\":\"echo/echo-1\"}",
        );
        self.put("source", "data/skills/portable/SKILL.md", "---\nname: portable\ndescription: Portable fixture skill\n---\nCheck the configuration.\n");
        self.put(
            "source",
            "workspace/.haarness/commands/portable-check.md",
            "---\ndescription: Portable check\n---\nInspect the configuration.\n",
        );
        self.put(
            "source",
            "workspace/AGENTS.md",
            "Portable project instructions.",
        );
        self.put(
            "source",
            "config/auth.json",
            "{\"fixture\":\"do-not-export\"}",
        );
        self.put("source", "data/haarness.db", "do-not-export-history");
        self.put("source", "config/scheduler/job.json", "do-not-register");
    }
    async fn start(&mut self, port: u16, token: &str) -> String {
        let logs = fs::File::create(self.root.join("server.log")).unwrap();
        let child = self
            .command("cloud")
            .args(["serve", "--port", &port.to_string(), "--token", token])
            .stdout(Stdio::null())
            .stderr(logs)
            .spawn()
            .unwrap();
        self.server = Some(child);
        let url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        for _ in 0..200 {
            if client
                .get(format!("{url}/v1/info"))
                .bearer_auth(token)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return url;
            }
            if let Some(status) = self.server.as_mut().unwrap().try_wait().unwrap() {
                panic!(
                    "server exited {status}: {}",
                    fs::read_to_string(self.root.join("server.log")).unwrap()
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "server startup timeout: {}",
            fs::read_to_string(self.root.join("server.log")).unwrap()
        );
    }
    async fn stop(&mut self, url: &str, token: &str) {
        reqwest::Client::new()
            .post(format!("{url}/v1/shutdown"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        for _ in 0..100 {
            if self.server.as_mut().unwrap().try_wait().unwrap().is_some() {
                self.server = None;
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("server did not stop");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.server {
            let _ = child.kill();
            let _ = child.wait();
        }
        for name in ["source", "target", "cloud"] {
            let _ = self.output(name, &["serve", "--stop"]);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Every command the fixture runs reads a home of its own, never the
/// person's: their Cloud sign-in, Claude Code folder and data folder stay out.
#[test]
fn the_fixture_runs_haarness_with_a_home_of_its_own() {
    use std::ffi::OsStr;
    let fixture = Fixture::new();
    let command = fixture.command("source");
    let env: std::collections::HashMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "HAARNESS_HOME",
    ] {
        let value = env.get(OsStr::new(name)).copied().flatten();
        assert!(
            value.is_some_and(|path| PathBuf::from(path).starts_with(&fixture.root)),
            "{name}: {value:?}"
        );
    }
    assert_eq!(
        env.get(OsStr::new("HAARNESS_TELEMETRY")),
        Some(&Some(OsStr::new("0")))
    );
    for removed in ["HAARNESS_DATA_DIR", "HAARNESS_CLOUD_CONFIG_DIR"] {
        assert_eq!(env.get(OsStr::new(removed)), Some(&None), "{removed}");
    }
}

#[test]
fn real_cli_save_inspect_dry_run_restore_conflict_force_and_runtime_agent_discovery() {
    let fixture = Fixture::new();
    fixture.seed();
    let archive = fixture.archive();
    let file = archive.to_str().unwrap();
    let saved = fixture.ok(
        "source",
        &[
            "profile",
            "save",
            "Portable engineering",
            "--output",
            file,
            "--json",
        ],
    );
    assert_eq!(
        saved["manifest"]["entries"].as_array().unwrap().len(),
        7,
        "paths: {:?}",
        saved["manifest"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        !fixture.root.join("source/data/workspaces.json").exists(),
        "save does not register workspaces"
    );
    assert!(
        !fixture.root.join("source/data/logs").exists(),
        "save does not start runtime logging"
    );
    let manifest = fixture.ok("target", &["profile", "inspect", file, "--json"]);
    assert_eq!(manifest["name"], "Portable engineering");
    let dry = fixture.ok(
        "target",
        &["profile", "restore", file, "--dry-run", "--json"],
    );
    assert_eq!(dry["dryRun"], true);
    assert!(!fixture.root.join("target/config/haarness.json").exists());
    fixture.ok("target", &["profile", "restore", file, "--json"]);
    for relative in [
        "config/haarness.json",
        "config/agents/portable-review.md",
        "config/skills/portable/SKILL.md",
        "workspace/AGENTS.md",
        "workspace/.haarness/commands/portable-check.md",
    ] {
        assert_eq!(
            fs::read(fixture.root.join("source").join(relative)).unwrap(),
            fs::read(fixture.root.join("target").join(relative)).unwrap()
        );
    }
    assert!(!fixture.root.join("target/config/auth.json").exists());
    assert!(!fixture.root.join("target/data/haarness.db").exists());
    fixture.put(
        "target",
        "config/haarness.json",
        "{\"model\":\"echo/alternate\"}",
    );
    let failed = fixture.output("target", &["profile", "restore", file]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("overwrite"));
    assert!(
        fs::read_to_string(fixture.root.join("target/config/haarness.json"))
            .unwrap()
            .contains("alternate")
    );
    fixture.ok("target", &["profile", "restore", file, "--force", "--json"]);
    let agents = fixture.ok("target", &["agents", "--json"]);
    assert!(
        agents
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| agent["id"] == "portable-review"),
        "restored custom agent is discovered: {agents}"
    );
    let output = fixture.output(
        "target",
        &[
            "--trust-project",
            "run",
            "--model",
            "echo/echo-1",
            "Portable configuration smoke",
        ],
    );
    assert!(
        output.status.success(),
        "restored runtime failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plugins = fixture.output("target", &["plugin", "list"]);
    assert!(plugins.status.success());
    assert!(String::from_utf8_lossy(&plugins.stdout).contains("portable-plugin"));
    let command = fixture.output(
        "target",
        &[
            "--trust-project",
            "run",
            "--model",
            "echo/echo-1",
            "--command",
            "portable-plugin:check",
        ],
    );
    assert!(
        command.status.success(),
        "restored plugin command failed: {}",
        String::from_utf8_lossy(&command.stderr)
    );
    // Scope selection is exercised through the real parser as well as archive-level tests.
    for scope in ["global", "local"] {
        let output = fixture.root.join(format!("{scope}.zip"));
        let saved = fixture.ok(
            "source",
            &[
                "profile",
                "save",
                scope,
                "--scope",
                scope,
                "--output",
                output.to_str().unwrap(),
                "--json",
            ],
        );
        assert!(
            saved["manifest"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["path"]
                    .as_str()
                    .unwrap()
                    .starts_with(if scope == "global" {
                        "global/"
                    } else {
                        "project/"
                    }))
        );
    }
}

#[tokio::test]
async fn real_cloud_upload_discovery_download_restore_auth_and_restart_persistence() {
    let mut fixture = Fixture::new();
    fixture.seed();
    let file = fixture.archive();
    fixture.ok(
        "source",
        &[
            "profile",
            "save",
            "Cloud engineering",
            "--output",
            file.to_str().unwrap(),
            "--json",
        ],
    );
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    let token = uuid::Uuid::now_v7().to_string();
    let url = fixture.start(port, &token).await;
    let client = reqwest::Client::new();
    let api = format!("{url}/v1/profiles");
    for request in [client.get(&api), client.post(&api).body("invalid")] {
        assert_eq!(request.send().await.unwrap().status(), 401);
    }
    assert_eq!(
        client
            .get(&api)
            .bearer_auth(&token)
            .header("Origin", "https://unrelated.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(&api)
            .bearer_auth(&token)
            .body("invalid")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let remote = |tail: &[&str]| {
        let mut args = vec!["profile", "--url", &url, "--token", &token, "--json"];
        args.extend_from_slice(tail);
        fixture.ok("target", &args)
    };
    let uploaded = remote(&["push", file.to_str().unwrap()]);
    let id = uploaded["id"].as_str().unwrap().to_owned();
    let listed = remote(&["search", "--query", "ENGINEERING"]);
    assert_eq!(listed[0]["id"], id);
    assert!(
        remote(&["search", "--offset", "1"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(remote(&["info", &id])["sha256"], uploaded["sha256"]);
    let downloaded = fixture.root.join("downloaded.zip");
    remote(&[
        "download",
        &id,
        "--output",
        downloaded.to_str().unwrap(),
    ]);
    assert_eq!(fs::read(&downloaded).unwrap(), fs::read(&file).unwrap());
    remote(&["pull", &id, "--dry-run"]);
    assert!(!fixture.root.join("target/config/haarness.json").exists());
    remote(&["pull", &id]);
    assert_eq!(
        fs::read(fixture.root.join("target/config/haarness.json")).unwrap(),
        fs::read(fixture.root.join("source/config/haarness.json")).unwrap()
    );
    assert!(
        !fixture.root.join("cloud/config/haarness.json").exists(),
        "upload cannot change server config"
    );
    let schema: Value = client
        .get(format!("{url}/openapi.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(schema["paths"]["/v1/profiles"]["post"].is_object());
    // Save-and-upload also uses the real CLI; a second archive has an independent immutable ID.
    let second = fixture.root.join("second.zip");
    let saved = remote(&[
        "save",
        "Second setup",
        "--output",
        second.to_str().unwrap(),
        "--push",
    ]);
    assert_ne!(saved["pushed"]["id"], id);
    fixture.stop(&url, &token).await;
    let url = fixture.start(port, &token).await;
    let listed = fixture.ok(
        "target",
        &[
            "profile",
            "--url",
            &url,
            "--token",
            &token,
            "search",
            "--json",
        ],
    );
    assert_eq!(listed.as_array().unwrap().len(), 2);
    fixture.ok(
        "target",
        &[
            "profile",
            "--url",
            &url,
            "--token",
            &token,
            "delete",
            &id,
            "--json",
        ],
    );
    assert_eq!(
        client
            .get(format!("{url}/v1/profiles/{id}/download"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let denied = fixture.output(
        "target",
        &[
            "profile",
            "--url",
            &url,
            "--token",
            "wrong-token-fixture",
            "search",
        ],
    );
    assert!(!denied.status.success());
    assert!(!String::from_utf8_lossy(&denied.stderr).contains("wrong-token-fixture"));
    let insecure = fixture.output(
        "target",
        &[
            "profile",
            "--url",
            "http://example.com",
            "--token",
            &token,
            "search",
        ],
    );
    assert!(!insecure.status.success());
    fixture.stop(&url, &token).await;
}
