use super::*;

fn tool_call(
    sequence: u64,
    thread: &Thread,
    call_id: &str,
    name: &str,
    arguments: serde_json::Value,
) -> Item {
    message_item(
        sequence,
        thread,
        ItemData::ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            arguments,
            provider_metadata: serde_json::Value::Null,
        },
    )
}

fn tool_result(
    sequence: u64,
    thread: &Thread,
    call_id: &str,
    name: &str,
    output: &str,
    is_error: bool,
    metadata: serde_json::Value,
) -> Item {
    message_item(
        sequence,
        thread,
        ItemData::ToolResult {
            call_id: call_id.into(),
            name: name.into(),
            output: output.into(),
            is_error,
            metadata,
        },
    )
}

/// A command's colours and window title, and an MCP server's coloured
/// error, show as their text: no `[32m` or `]0;` remains of them, also
/// when the output is opened in full. Output that is nothing but escapes
/// reads as no output. The stored result keeps what the model read.
#[test]
fn tool_output_and_errors_show_without_their_escape_sequences() {
    let thread = fixture_thread();
    let coloured = "exit: 0\n\u{1b}]0;build logs\u{7}\u{1b}[32mpassed\u{1b}[39m 12 tests\n\u{1b}[1mtotal\u{1b}[22m 0.4s";
    let items = vec![
        tool_call(
            1,
            &thread,
            "run-1",
            "bash",
            serde_json::json!({"command": "npm test"}),
        ),
        tool_result(
            2,
            &thread,
            "run-1",
            "bash",
            coloured,
            false,
            serde_json::json!({"exit": 0}),
        ),
        tool_call(
            3,
            &thread,
            "mcp-1",
            "mcp__wiki__lookup",
            serde_json::json!({}),
        ),
        tool_result(
            4,
            &thread,
            "mcp-1",
            "mcp__wiki__lookup",
            "\u{1b}[31mFailed:\u{1b}[39m page \u{1b}]8;;https://example.org/p\u{1b}\\moved\u{1b}]8;;\u{1b}\\",
            true,
            serde_json::Value::Null,
        ),
        tool_call(
            5,
            &thread,
            "run-2",
            "bash",
            serde_json::json!({"command": "tput init"}),
        ),
        tool_result(
            6,
            &thread,
            "run-2",
            "bash",
            "exit: 0\n\u{1b}(B\u{1b}[m",
            false,
            serde_json::json!({"exit": 0}),
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    app.tool_details = true;
    let text = frame_text(&app, (100, 40));
    assert!(text.contains("passed 12 tests"), "{text}");
    assert!(text.contains("total 0.4s"), "{text}");
    assert!(text.contains("Failed: page moved"), "{text}");
    for remains in [
        "[32m",
        "[39m",
        "[1m",
        "]0;",
        "build logs",
        "]8;",
        "(B",
        "[m",
    ] {
        assert!(!text.contains(remains), "{remains} in {text}");
    }
    // Collapsed, the escape-only output says so on its row.
    app.tool_details = false;
    let text = frame_text(&app, (100, 40));
    assert!(text.contains("no output"), "{text}");
    assert!(text.contains("Failed: page moved"), "{text}");
    assert!(!text.contains("[31m"), "{text}");
    let ItemData::ToolResult { output, .. } = &app.items[1].data else {
        panic!("a result");
    };
    assert_eq!(output, coloured, "the model's text is unchanged");
}

/// A direction override, a zero-width space and a carriage return in a
/// command show on the card as codes in the muted color, in the command, in
/// the rule Always saves and in the one-line summary, in an RTL language
/// too; the call's row above the card shows the same codes. The request
/// itself, which the answer refers to, is unchanged.
#[tokio::test]
async fn an_approval_shows_what_is_invisible_in_it_as_quiet_codes() {
    let thread = fixture_thread();
    let command = "cat report\u{202e}fdp.sh\u{200b} && cd dist\r\nrm -r cache";
    let call = tool_call(
        1,
        &thread,
        "call-hidden",
        "bash",
        serde_json::json!({"command": command}),
    );
    let mut app = App::new(
        thread.clone(),
        vec![thread.clone()],
        vec![call],
        vec![],
        vec![],
    );
    let rule = "bash:cat report\u{202e}*";
    let event = fixture_event(
        "permission.requested",
        thread.id,
        thread.id,
        serde_json::json!({
            "permissionId": "perm-hidden",
            "permission": "bash",
            "operation": format!("bash:{command}"),
            "patterns": [command],
            "alwaysPatterns": [rule],
            "detail": {"command": command},
            "callId": "call-hidden",
        }),
    );
    app.reduce(event, &offline_client()).await.unwrap();
    let hidden = ['\u{202e}', '\u{200b}', '\r', '\u{2061}', '\u{2062}'];
    for language in [
        crate::locale::Language::English,
        crate::locale::Language::Arabic,
    ] {
        app.language = language;
        let text: String = app
            .lines((100, 40))
            .iter()
            .map(|line| line_text(line) + "\n")
            .collect();
        assert!(
            !text.contains(hidden),
            "nothing invisible reaches the terminal: {text:?}"
        );
        assert!(
            text.contains("bash cat report\\u{202E}fdp.sh\\u{200B} && cd dist\\u{D}"),
            "the call's row: {text}"
        );
        // The card's own rows: the command, its codes quiet and on their
        // own, and the rule Always would save.
        let card = crate::view::approval::approval_lines(&app, 100, 30).lines;
        let card_text: String = card.iter().map(|line| line_text(line) + "\n").collect();
        assert!(
            card_text.contains("$ cat report\\u{202E}fdp.sh\\u{200B} && cd dist\\u{D}"),
            "{card_text}"
        );
        assert!(card_text.contains("  rm -r cache"), "{card_text}");
        assert!(card_text.contains("cat report\\u{202E}*"), "{card_text}");
        assert!(!card_text.contains(hidden), "{card_text:?}");
        let spans: Vec<_> = card.iter().flat_map(|line| &line.spans).collect();
        for code in ["\\u{202E}", "\\u{200B}", "\\u{D}"] {
            let quiet = spans
                .iter()
                .filter(|span| span.text.contains(code))
                .collect::<Vec<_>>();
            assert!(!quiet.is_empty(), "{code}: {spans:#?}");
            assert!(
                quiet
                    .iter()
                    .all(|span| span.text == code && span.fg == app.theme.muted),
                "{code} is quiet on its own: {spans:#?}"
            );
        }
        assert!(
            spans
                .iter()
                .any(|span| span.text == "cat report" && span.fg == app.theme.text),
            "{spans:#?}"
        );
    }
    let pending = &app.pending_permissions[0];
    assert_eq!(pending.always_patterns, [rule]);
    assert_eq!(pending.detail["command"], command);
    let summary = crate::view::approval::summary(pending, crate::locale::Language::English);
    assert!(
        summary.ends_with("cat report\\u{202E}fdp.sh\\u{200B} && cd dist\\u{D}"),
        "{summary}"
    );
}

/// An ordinary command reads on its card exactly as written: no codes, one
/// span of text per row, whatever the script or emoji in it.
#[tokio::test]
async fn an_ordinary_command_reads_on_its_card_as_written() {
    let thread = fixture_thread();
    let command = "git commit -m \"修复: نص عربي 👩\u{200d}💻 café\"\nnpm test";
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let event = fixture_event(
        "permission.requested",
        thread.id,
        thread.id,
        serde_json::json!({
            "permissionId": "perm-plain",
            "permission": "bash",
            "operation": format!("bash:{command}"),
            "patterns": [command],
            "alwaysPatterns": ["bash:git commit *"],
            "detail": {"command": command},
        }),
    );
    app.reduce(event, &offline_client()).await.unwrap();
    let card = crate::view::approval::approval_lines(&app, 100, 30).lines;
    let card_text: String = card.iter().map(|line| line_text(line) + "\n").collect();
    assert!(
        card_text.contains("$ git commit -m \"修复: نص عربي 👩\u{200d}💻 café\""),
        "{card_text}"
    );
    assert!(card_text.contains("  npm test"), "{card_text}");
    assert!(!card_text.contains("\\u{"), "{card_text}");
    let row = card
        .iter()
        .find(|line| line_text(line).contains("git commit"))
        .unwrap();
    assert!(
        row.spans
            .iter()
            .filter(|span| span.selectable && !span.text.trim().is_empty())
            .all(|span| span.fg == app.theme.text),
        "{row:#?}"
    );
}

/// An edit's changed lines show a direction override or isolate as a muted
/// code, so a reordered line cannot read innocently on its card; the
/// file's right-to-left text and its marks read as written.
#[tokio::test]
async fn an_edit_on_its_card_shows_what_turns_a_line_around() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let new = "if (role != \"user\u{202e} \u{2066}// admin\u{2069} \u{2066}\") {\nlet greeting = \"مرحبا\u{200f}\";\n";
    let event = fixture_event(
        "permission.requested",
        thread.id,
        thread.id,
        serde_json::json!({
            "permissionId": "perm-edit",
            "permission": "edit",
            "operation": "edit:src/access.js",
            "patterns": ["src/access.js"],
            "alwaysPatterns": ["edit:src/*"],
            "detail": {"path": "src/access.js", "old": "if (role != \"user\") {\n", "new": new},
        }),
    );
    app.reduce(event, &offline_client()).await.unwrap();
    let card = crate::view::approval::approval_lines(&app, 100, 30).lines;
    let card_text: String = card.iter().map(|line| line_text(line) + "\n").collect();
    assert!(
        card_text
            .contains("+ if (role != \"user\\u{202E} \\u{2066}// admin\\u{2069} \\u{2066}\") {"),
        "{card_text}"
    );
    assert!(
        card_text.contains("+ let greeting = \"مرحبا\u{200f}\";"),
        "{card_text}"
    );
    assert!(
        !card_text.contains(['\u{202e}', '\u{2066}', '\u{2069}', '\u{2061}', '\u{2062}']),
        "{card_text:?}"
    );
    let spans: Vec<_> = card.iter().flat_map(|line| &line.spans).collect();
    assert!(
        spans
            .iter()
            .filter(|span| span.text.contains("\\u{"))
            .all(|span| span.text.starts_with("\\u{") && span.fg == app.theme.muted),
        "{spans:#?}"
    );
}

/// A failed workflow's error, an agent's error and its logs read without
/// the escape sequences a program left in them.
#[test]
fn a_workflow_run_reads_its_errors_and_logs_without_escape_sequences() {
    let thread = fixture_thread();
    let app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let mut run = fixture_workflow_run("nightly", haarness_protocol::WorkflowRunStatus::Failed);
    run.error = Some("\u{1b}[31mstep 3 failed\u{1b}[39m: lint".into());
    run.logs[0].message = "\u{1b}]0;runner\u{7}\u{1b}[1mchecking\u{1b}[22m 4 files".into();
    run.agents = vec![
        serde_json::from_value(serde_json::json!({
            "id": "agent-1",
            "label": "linter",
            "status": "failed",
            "error": "\u{1b}[33mwarning:\u{1b}[0m model refused",
            "startedAt": run.time.created,
        }))
        .unwrap(),
    ];
    let body = crate::view::workflow::run_body(&app, &run, 100);
    let text = body.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("step 3 failed: lint"), "{text}");
    assert!(text.contains("checking 4 files"), "{text}");
    assert!(text.contains("warning: model refused"), "{text}");
    for remains in [
        "[31m",
        "[1m",
        "[33m",
        "]0;",
        "runner",
        '\u{1b}'.to_string().as_str(),
    ] {
        assert!(!text.contains(remains), "{remains:?} in {text}");
    }
}

/// A patch's row names its file with what would not show as a code, as
/// the other rows do.
#[test]
fn a_patch_row_names_its_file_with_codes() {
    let thread = fixture_thread();
    let patch =
        "*** Begin Patch\n*** Update File: src/notes\u{202e}txt.js\n@@\n-a\n+b\n*** End Patch\n";
    let items = vec![tool_call(
        1,
        &thread,
        "patch-1",
        "apply_patch",
        serde_json::json!({"input": patch}),
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    let text = frame_text(&app, (100, 20));
    assert!(
        text.contains("apply_patch src/notes\\u{202E}txt.js"),
        "{text}"
    );
    assert!(!text.contains('\u{202e}'), "{text:?}");
}

/// A result's output is cleaned once and kept: the row reads the kept text
/// on later frames rather than cleaning the stored output again.
#[test]
fn a_result_is_cleaned_once_and_read_from_then_on() {
    let thread = fixture_thread();
    let items = vec![
        tool_call(
            1,
            &thread,
            "run-1",
            "bash",
            serde_json::json!({"command": "make"}),
        ),
        tool_result(
            2,
            &thread,
            "run-1",
            "bash",
            "exit: 1\n\u{1b}[31mbuild failed\u{1b}[39m",
            false,
            serde_json::json!({"exit": 1}),
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    let text = frame_text(&app, (100, 20));
    assert!(text.contains("build failed"), "{text}");
    let id = app.items[1].id;
    assert_eq!(
        app.shown_outputs
            .borrow()
            .get(&id)
            .cloned()
            .flatten()
            .as_deref(),
        Some("build failed")
    );
    app.shown_outputs
        .borrow_mut()
        .insert(id, Some("kept from the first frame".into()));
    let text = frame_text(&app, (100, 20));
    assert!(text.contains("kept from the first frame"), "{text}");
}
