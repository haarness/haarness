use haarness_language::{
    FormatterConfig, FormatterDefinition, LanguageService, LspConfig, LspServerConfig, Operation,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("haarness-language-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

fn server(extensions: &[&str]) -> LspServerConfig {
    LspServerConfig {
        command: vec!["python3".into(), fixture("mock_lsp.py")],
        extensions: extensions.iter().map(|value| (*value).into()).collect(),
        language_ids: BTreeMap::from([
            ("rs".into(), "rust".into()),
            ("ts".into(), "typescript".into()),
            ("py".into(), "python".into()),
            ("go".into(), "go".into()),
            ("mock".into(), "mock-language".into()),
        ]),
        ..LspServerConfig::default()
    }
}

fn formatter(extensions: &[&str]) -> FormatterDefinition {
    FormatterDefinition {
        command: vec![
            "python3".into(),
            fixture("mock_formatter.py"),
            "$FILE".into(),
        ],
        extensions: extensions.iter().map(|value| (*value).into()).collect(),
        ..FormatterDefinition::default()
    }
}

fn service(workspace: &Path, enabled: bool) -> Arc<LanguageService> {
    let extensions = [".rs", ".ts", ".py", ".go", ".mock"];
    Arc::new(LanguageService::new(
        workspace.to_path_buf(),
        LspConfig {
            enabled,
            builtins: false,
            diagnostic_timeout_ms: 2_000,
            request_timeout_ms: 2_000,
            servers: BTreeMap::from([("mock".into(), server(&extensions))]),
            ..LspConfig::default()
        },
        FormatterConfig {
            enabled,
            builtins: false,
            formatters: BTreeMap::from([("mockfmt".into(), formatter(&extensions))]),
        },
    ))
}

#[tokio::test]
async fn formats_diagnoses_and_queries_across_language_matrix() {
    let workspace = Workspace::new();
    let service = service(&workspace.0, true);
    let cancel = CancellationToken::new();
    let samples = [
        ("sample.rs", "fn main() { /* BADFMT BROKEN */ }\n"),
        ("sample.ts", "const value: number = 1 // BADFMT BROKEN\n"),
        ("sample.py", "value = 1  # BADFMT BROKEN\n"),
        ("sample.go", "package main // BADFMT BROKEN\n"),
        ("sample.mock", "entity BADFMT BROKEN\n"),
    ];

    for (name, contents) in samples {
        let path = workspace.0.join(name);
        tokio::fs::write(&path, contents).await.unwrap();
        let report = service.process_file(path.clone(), cancel.clone()).await;
        assert_eq!(report.formatted_by, ["mockfmt"]);
        assert_eq!(report.diagnostics.len(), 1, "{name}");
        assert_eq!(report.diagnostics[0].code.as_deref(), Some("mock-error"));
        let formatted = tokio::fs::read_to_string(path).await.unwrap();
        assert!(formatted.contains("GOODFMT"), "{name}");
        assert!(!formatted.contains("BADFMT"), "{name}");
    }

    let source = workspace.0.join("sample.rs");
    for operation in [
        Operation::Definition,
        Operation::References,
        Operation::Hover,
        Operation::DocumentSymbols,
        Operation::WorkspaceSymbols,
        Operation::Implementation,
        Operation::PrepareCallHierarchy,
        Operation::IncomingCalls,
        Operation::OutgoingCalls,
    ] {
        let result = service
            .query(operation, &source, 0, 0, None, &cancel)
            .await
            .unwrap();
        assert_eq!(result["results"][0]["server"], "mock");
        assert!(!result["results"][0]["result"].is_null());
    }
    assert_eq!(service.status().await[0].status, "running");
    assert_eq!(service.formatter_status()[0].status, "available");
    service.shutdown().await;
    assert_eq!(service.status().await[0].status, "stopped");
}

#[tokio::test]
async fn global_disable_controls_prevent_processes_and_formatting() {
    let workspace = Workspace::new();
    let source = workspace.0.join("sample.rs");
    tokio::fs::write(&source, "BADFMT BROKEN\n").await.unwrap();
    let service = service(&workspace.0, false);
    let report = service
        .process_file(source.clone(), CancellationToken::new())
        .await;
    assert!(report.formatted_by.is_empty());
    assert!(report.diagnostics.is_empty());
    assert_eq!(
        tokio::fs::read_to_string(source).await.unwrap(),
        "BADFMT BROKEN\n"
    );
    assert!(service.status().await.is_empty());
    assert!(service.formatter_status().is_empty());
}

#[tokio::test]
async fn replacement_generation_updates_lsp_and_formatter_catalogs_live() {
    let workspace = Workspace::new();
    let service = service(&workspace.0, false);
    assert!(service.status().await.is_empty());
    assert!(service.formatter_status().is_empty());

    let extensions = [".mock"];
    service.replace_config(
        LspConfig {
            enabled: true,
            builtins: false,
            servers: BTreeMap::from([("replacement".into(), server(&extensions))]),
            ..LspConfig::default()
        },
        FormatterConfig {
            enabled: true,
            builtins: false,
            formatters: BTreeMap::from([("replacement".into(), formatter(&extensions))]),
        },
    );
    assert_eq!(service.status().await[0].id, "replacement");
    assert_eq!(service.formatter_status()[0].id, "replacement");
}

#[tokio::test]
async fn per_entry_disable_and_missing_commands_fail_open() {
    let workspace = Workspace::new();
    let source = workspace.0.join("sample.mock");
    tokio::fs::write(&source, "unchanged\n").await.unwrap();
    let disabled = LspServerConfig {
        enabled: false,
        ..server(&[".mock"])
    };
    let missing = LspServerConfig {
        command: vec!["haarness-definitely-missing-lsp".into()],
        extensions: vec![".mock".into()],
        ..LspServerConfig::default()
    };
    let service = LanguageService::new(
        workspace.0.clone(),
        LspConfig {
            builtins: false,
            servers: BTreeMap::from([("disabled".into(), disabled), ("missing".into(), missing)]),
            ..LspConfig::default()
        },
        FormatterConfig {
            enabled: true,
            builtins: false,
            formatters: BTreeMap::from([
                (
                    "disabled".into(),
                    FormatterDefinition {
                        enabled: false,
                        ..formatter(&[".mock"])
                    },
                ),
                (
                    "missing".into(),
                    FormatterDefinition {
                        command: vec!["haarness-definitely-missing-formatter".into()],
                        extensions: vec![".mock".into()],
                        ..FormatterDefinition::default()
                    },
                ),
            ]),
        },
    );
    assert_eq!(service.status().await.len(), 1);
    assert_eq!(service.status().await[0].status, "missing");
    assert_eq!(service.formatter_status().len(), 1);
    assert_eq!(service.formatter_status()[0].status, "missing");
    let report = service
        .process_file(source.clone(), CancellationToken::new())
        .await;
    assert!(report.formatted_by.is_empty());
    assert!(report.diagnostics.is_empty());
    assert_eq!(
        tokio::fs::read_to_string(source).await.unwrap(),
        "unchanged\n"
    );
    // Trying to launch it does not turn "not installed" into a raw OS error.
    let status = service.status().await;
    assert_eq!(status[0].status, "missing");
    assert_eq!(status[0].error, None);
}

#[tokio::test]
async fn per_server_timeouts_override_global_limits_and_remain_cancellable() {
    let workspace = Workspace::new();
    let file = workspace.0.join("example.mock");
    std::fs::write(&file, "example").unwrap();
    let mut slow = server(&[".mock"]);
    slow.environment.insert("INIT_DELAY".into(), "0.15".into());
    slow.environment
        .insert("REQUEST_DELAY".into(), "0.15".into());
    slow.initialize_timeout_ms = Some(2000);
    slow.request_timeout_ms = Some(2000);
    let service = LanguageService::new(
        workspace.0.clone(),
        LspConfig {
            builtins: false,
            request_timeout_ms: 1,
            servers: BTreeMap::from([("slow".into(), slow)]),
            ..LspConfig::default()
        },
        FormatterConfig::default(),
    );
    let cancel = CancellationToken::new();
    assert!(
        service
            .query(Operation::Hover, &file, 0, 0, None, &cancel)
            .await
            .is_ok()
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        service
            .query(Operation::Hover, &file, 0, 0, None, &cancelled)
            .await
            .is_err()
    );
    service.shutdown().await;
}

#[tokio::test]
async fn idle_clients_are_reaped_and_restart_on_demand() {
    let workspace = Workspace::new();
    let file = workspace.0.join("sample.mock");
    std::fs::write(&file, "hello").unwrap();
    let service = LanguageService::new(
        workspace.0.clone(),
        LspConfig {
            builtins: false,
            idle_timeout_ms: 50,
            servers: BTreeMap::from([("mock".into(), server(&[".mock"]))]),
            ..LspConfig::default()
        },
        FormatterConfig {
            enabled: false,
            ..FormatterConfig::default()
        },
    );
    let cancel = CancellationToken::new();
    service
        .query(Operation::Hover, &file, 0, 0, None, &cancel)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if service.status().await[0].status == "idle" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    service
        .query(Operation::Hover, &file, 0, 0, None, &cancel)
        .await
        .unwrap();
    assert_eq!(service.status().await[0].status, "running");
    service.shutdown().await;
}

#[tokio::test]
async fn capacity_reaps_idle_clients_but_preserves_inflight_requests() {
    let workspace = Workspace::new();
    let first = workspace.0.join("a.first");
    let second = workspace.0.join("b.second");
    std::fs::write(&first, "hello").unwrap();
    std::fs::write(&second, "hello").unwrap();
    let mut slow = server(&[".first"]);
    slow.environment
        .insert("REQUEST_DELAY".into(), "0.2".into());
    let service = Arc::new(LanguageService::new(
        workspace.0.clone(),
        LspConfig {
            builtins: false,
            idle_timeout_ms: 50,
            max_clients: 1,
            servers: BTreeMap::from([
                ("first".into(), slow),
                ("second".into(), server(&[".second"])),
            ]),
            ..LspConfig::default()
        },
        FormatterConfig {
            enabled: false,
            ..FormatterConfig::default()
        },
    ));
    let worker = service.clone();
    let active = tokio::spawn(async move {
        worker
            .query(
                Operation::Hover,
                &first,
                0,
                0,
                None,
                &CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if service.status().await[0].status == "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        service
            .query(
                Operation::Hover,
                &second,
                0,
                0,
                None,
                &CancellationToken::new()
            )
            .await
            .unwrap_err()
            .contains("capacity")
    );
    assert!(
        active.await.unwrap().is_ok(),
        "idle sweeper killed an active request"
    );
    service
        .query(
            Operation::Hover,
            &second,
            0,
            0,
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let statuses = service.status().await;
    assert_eq!(statuses[0].status, "idle");
    assert_eq!(statuses[1].status, "running");
    service.shutdown().await;
}
