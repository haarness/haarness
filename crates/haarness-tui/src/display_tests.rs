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
