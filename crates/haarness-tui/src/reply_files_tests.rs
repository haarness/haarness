use super::*;
use crate::reply_files::{
    ClickCheck, Launch, Reference, code_reference, inside_project, launch, link_reference,
    project_file,
};

/// A long conversation whose answers mention files the way models write them.
fn long_conversation(thread: &Thread, replies: usize) -> Vec<Item> {
    (0..replies)
        .flat_map(|index| {
            let sequence = u64::try_from(index).unwrap() * 2;
            [
                message_item(
                    sequence + 1,
                    thread,
                    ItemData::UserMessage {
                        text: format!("question {index}"),
                        attachments: Vec::new(),
                        queued: false,
                        synthetic: false,
                        command_line: None,
                        hook: None,
                    },
                ),
                message_item(
                    sequence + 2,
                    thread,
                    ItemData::AssistantMessage {
                        text: format!(
                            "Reply {index}: the change is in `src/app.rs:{index}` and \
                             `src/lib/util.rs`, next to `cargo test` and `foo()`. See \
                             [the notes](docs/notes.md#L3) and [the site](https://example.com).\n\n\
                             - one `README.md`\n- two `src/missing.rs:9`\n"
                        ),
                    },
                ),
            ]
        })
        .collect()
}

/// Frame time on a long transcript, typed into and scrolled: run before and
/// after a change to the transcript's drawing to see what it costs.
/// `cargo test -p haarness-tui --release reply_files_tests::frame_time -- --ignored --nocapture`
#[test]
#[ignore = "a measurement, not a check"]
fn frame_time_on_a_long_transcript() {
    let project = Project::new();
    let thread = fixture_thread();
    let items = long_conversation(&thread, 600);
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    local(&mut app, &project);
    let size = (140, 48);
    let _ = app.lines(size);
    let frames = 300;
    let started = std::time::Instant::now();
    for index in 0..frames {
        app.composer.set_text(format!("typing {index}"));
        let _ = app.lines(size);
    }
    let typing = started.elapsed() / frames;
    let started = std::time::Instant::now();
    for index in 0..frames {
        app.scroll(if index % 60 < 30 { 7 } else { -7 });
        let _ = app.lines(size);
    }
    let scrolling = started.elapsed() / frames;
    println!("frame while typing: {typing:?}; while scrolling: {scrolling:?}");
}

/// A project folder the test makes and removes: `src/app.rs` (60 lines),
/// `src/lib/util.rs`, `docs/notes.md`, a file called `--cmd=x.rs`, and on
/// Unix `src/outside.rs`, a symbolic link to a file next to the project.
struct Project {
    folder: std::path::PathBuf,
    root: std::path::PathBuf,
}

impl Project {
    fn new() -> Self {
        let folder =
            std::env::temp_dir().join(format!("haarness-reply-files-{}", uuid::Uuid::now_v7()));
        let root = folder.join("project");
        std::fs::create_dir_all(root.join("src/lib")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let lines: String =
            (1..=60).fold(String::new(), |text, line| text + &format!("line {line}\n"));
        std::fs::write(root.join("src/app.rs"), lines).unwrap();
        std::fs::write(root.join("src/lib/util.rs"), "util\n").unwrap();
        std::fs::write(root.join("docs/notes.md"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(root.join("--cmd=x.rs"), "flag\n").unwrap();
        std::fs::write(folder.join("outside.rs"), "outside\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(folder.join("outside.rs"), root.join("src/outside.rs")).unwrap();
        Self { folder, root }
    }

    fn file(&self, path: &str) -> std::path::PathBuf {
        dunce::canonicalize(self.root.join(path)).unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

/// Point `app` at a server on this computer serving `project`.
fn local(app: &mut App, project: &Project) {
    let mut info = side_server_info(Vec::new());
    info.workspace = Some(project.root.display().to_string());
    app.server_info = Some(info);
    app.files_local = true;
}

#[allow(clippy::unnecessary_wraps)] // compared with what the parsers return
fn reference(path: &str, line: Option<u32>) -> Option<Reference> {
    Some(Reference {
        path: path.into(),
        line,
    })
}

#[test]
fn code_shaped_like_a_path_gives_a_reference() {
    for (code, path, line) in [
        ("src/app.rs", "src/app.rs", None),
        ("src/app.rs:42", "src/app.rs", Some(42)),
        (" src/app.rs:42:7 ", "src/app.rs", Some(42)),
        ("src/app.rs:42-60", "src/app.rs", Some(42)),
        ("src/app.rs#L42", "src/app.rs", Some(42)),
        ("src/app.rs#L42-L60", "src/app.rs", Some(42)),
        ("src/app.rs:0", "src/app.rs", None),
        ("./src/app.rs", "./src/app.rs", None),
        ("README.md", "README.md", None),
        ("README.md:3", "README.md", Some(3)),
        (".env", ".env", None),
        ("app/[id]/page.tsx", "app/[id]/page.tsx", None),
        ("app/(site)/page.tsx:4", "app/(site)/page.tsx", Some(4)),
        ("(site)/about/page.tsx", "(site)/about/page.tsx", None),
        ("app/(.)photo/page.tsx", "app/(.)photo/page.tsx", None),
        (
            "node_modules/@types/node/index.d.ts",
            "node_modules/@types/node/index.d.ts",
            None,
        ),
        (
            "/work/project/src/app.rs:9",
            "/work/project/src/app.rs",
            Some(9),
        ),
        ("C:/project/app.rs:3", "C:/project/app.rs", Some(3)),
        ("scripts/build", "scripts/build", None),
    ] {
        assert_eq!(code_reference(code, false), reference(path, line), "{code}");
        assert_eq!(code_reference(code, true), reference(path, line), "{code}");
    }
    for code in [
        "",
        "npm test",
        "foo()",
        "run(x).rs",
        "app/run(x)/a.rs",
        "app/(a(b))/c.rs",
        "app/()/c.rs",
        "a = b",
        "--flag",
        "-x.rs",
        "~/notes.md",
        "@scope/pkg",
        "https://example.com/a.rs",
        "1.2.3",
        "v2.0",
        "Makefile",
        "src/",
        "$HOME/x.rs",
        "a,b.rs",
        "<div>",
        "std::io::Read",
        "key:value",
        "*.rs",
        "a\tb.rs",
    ] {
        assert_eq!(code_reference(code, false), None, "{code:?}");
        assert_eq!(code_reference(code, true), None, "{code:?}");
    }
}

/// Windows writes paths with backslashes; elsewhere a backslash is an escape
/// and the code is no path.
#[test]
fn backslashes_separate_folders_only_where_they_do() {
    for (code, path, line) in [
        ("src\\app.rs:3", "src\\app.rs", Some(3)),
        ("C:\\project\\app.rs", "C:\\project\\app.rs", None),
        ("app\\(site)\\page.tsx", "app\\(site)\\page.tsx", None),
    ] {
        assert_eq!(code_reference(code, true), reference(path, line), "{code}");
        assert_eq!(code_reference(code, false), None, "{code}");
    }
    assert_eq!(code_reference("src\\", true), None);
    assert_eq!(code_reference("main\\.rs", false), None);
}

#[test]
fn a_link_to_a_path_gives_a_reference_and_other_links_do_not() {
    for (href, path, line) in [
        ("src/app.rs", "src/app.rs", None),
        ("./src/app.rs", "./src/app.rs", None),
        ("src/app.rs:42", "src/app.rs", Some(42)),
        ("src/app.rs#L42-L50", "src/app.rs", Some(42)),
        ("README.md:3", "README.md", Some(3)),
        ("main.rs:42:7", "main.rs", Some(42)),
        ("README.md#install", "README.md", None),
        ("src/app.rs?plain", "src/app.rs", None),
        ("src/my%20app.rs:3", "src/my app.rs", Some(3)),
        ("--cmd=x.rs", "--cmd=x.rs", None),
        (
            "/work/project/src/app.rs:7",
            "/work/project/src/app.rs",
            Some(7),
        ),
    ] {
        assert_eq!(link_reference(href), reference(path, line), "{href}");
    }
    #[cfg(unix)]
    for (href, path, line) in [
        ("file:///work/project/a.rs", "/work/project/a.rs", None),
        (
            "file:///work/project/a.rs#L3-L4",
            "/work/project/a.rs",
            Some(3),
        ),
        ("file://localhost/work/a%20b.rs", "/work/a b.rs", None),
    ] {
        assert_eq!(link_reference(href), reference(path, line), "{href}");
    }
    for href in [
        "",
        "#heading",
        "#L5",
        "https://example.com/src/app.rs",
        "http://localhost:8080/a.rs",
        "//host/src/app.rs",
        "\\\\host\\share\\a.rs",
        "file://server/share/a.rs",
        "mailto:someone@example.com",
        "tel:123",
        "javascript:alert(1)",
        "x-example.settings:org.example.panel",
        "vscode://file/a.rs:3",
        "src/%zz.rs",
        "src/a%0a.rs",
    ] {
        assert_eq!(link_reference(href), None, "{href:?}");
    }
}

#[test]
fn an_encoded_reference_decodes_to_the_same_one() {
    for (path, line) in [
        ("src/app.rs", None),
        ("src/app.rs", Some(42)),
        ("C:/a:b/12:3.rs", Some(7)),
        ("12:34", None),
    ] {
        let written = Reference {
            path: path.into(),
            line,
        };
        assert_eq!(Reference::decode(&written.encode()), Some(written));
    }
    assert_eq!(Reference::decode("x:"), None);
    assert_eq!(Reference::decode("no colon"), None);
}

/// Only a regular file inside the project resolves, to its canonical path; a
/// missing file, a folder, a path outside and a symbolic link out do not.
#[test]
fn only_a_file_inside_the_project_resolves() {
    let project = Project::new();
    let resolved = dunce::canonicalize(&project.root).unwrap();
    let named = |path: &str| {
        project_file(
            &project.root,
            &resolved,
            &Reference {
                path: path.into(),
                line: None,
            },
        )
    };
    let app = project.file("src/app.rs");
    assert!(app.is_absolute());
    assert_eq!(named("src/app.rs"), Some(app.clone()));
    assert_eq!(named("./src/lib/../app.rs"), Some(app.clone()));
    assert_eq!(named(&app.display().to_string()), Some(app));
    assert_eq!(named("--cmd=x.rs"), Some(project.file("--cmd=x.rs")));
    for path in [
        "src/missing.rs",
        "src/lib",
        "../outside.rs",
        "src/outside.rs",
    ] {
        assert_eq!(named(path), None, "{path}");
    }
    let outside = project.folder.join("outside.rs");
    assert_eq!(named(&outside.display().to_string()), None);
}

#[test]
fn answers_keep_the_references_of_their_code_and_links() {
    let theme = crate::theme::Theme::editorial_dark();
    let lines = crate::markdown::render_markdown(
        "Change `src/app.rs:42`, run `cargo test`, read [the notes](docs/notes.md#L2) \
         and [the site](https://example.com).",
        200,
        &theme,
        "  ",
    );
    let spans: Vec<_> = lines.iter().flat_map(|line| &line.spans).collect();
    let opens = |text: &str| {
        spans
            .iter()
            .find(|span| span.text.contains(text))
            .unwrap_or_else(|| panic!("{text} not drawn"))
            .opens
            .clone()
    };
    assert_eq!(opens("src/app.rs:42").as_deref(), Some("42:src/app.rs"));
    // The code chip looks as it always has.
    let code = spans
        .iter()
        .find(|span| span.text.contains("src/app.rs"))
        .unwrap();
    assert_eq!(code.text, " src/app.rs:42 ");
    assert_eq!((code.fg, code.bg), (theme.accent, Some(theme.surface)));
    assert_eq!(opens("cargo test"), None);
    let notes = opens("the notes").unwrap();
    assert_eq!(&*notes, "2:docs/notes.md");
    // The path shown after the words opens the same file; its brackets do not.
    assert!(std::sync::Arc::ptr_eq(
        &notes,
        &opens("docs/notes.md#L2").unwrap()
    ));
    assert_eq!(opens(" ("), None);
    assert_eq!(opens("the site"), None);
    assert_eq!(opens("https://example.com"), None);
    assert!(text_of(&lines).contains("the notes (docs/notes.md#L2)"));
}

/// Shares on other computers and Windows device paths, written plainly or
/// escaped, never become references, and the text check refuses them before
/// anything could ask the disk, on every system.
#[test]
fn shares_and_device_paths_are_refused_before_the_disk() {
    for code in [
        "//host/s/a.rs",
        "\\\\host\\s\\a.rs",
        "/\\host\\s\\a.rs",
        "\\\\?\\C:\\a.rs",
    ] {
        assert_eq!(code_reference(code, true), None, "{code}");
        assert_eq!(code_reference(code, false), None, "{code}");
    }
    for href in [
        "//host/s/a.rs",
        "\\\\host\\s\\a.rs",
        "%5C%5Chost%5Cs%5Ca.rs",
        "%2F%2Fhost/s/a.rs",
        "%2f%5chost/s/a.rs",
        "/\\host\\s",
        "/%5Chost/s/a.rs",
        "file:////host/s/a.rs",
        "\\\\.\\pipe\\x.rs",
    ] {
        assert_eq!(link_reference(href), None, "{href}");
    }
    let root = std::path::Path::new("/work/project");
    for path in [
        "//host/s/a.rs",
        "\\\\host\\s\\a.rs",
        "/\\host\\s",
        "\\\\?\\C:\\work\\project\\a.rs",
        "\\\\.\\C:\\a.rs",
        "C:notes.md",
        "../outside.rs",
        "src/../../outside.rs",
        "/work/other/a.rs",
        "/work/project-two/a.rs",
    ] {
        assert_eq!(inside_project(path, root, root), None, "{path}");
    }
    for (path, inside) in [
        ("src/app.rs", "/work/project/src/app.rs"),
        ("./src/../README.md", "/work/project/README.md"),
        ("/work/project/src/app.rs", "/work/project/src/app.rs"),
    ] {
        assert_eq!(
            inside_project(path, root, root),
            Some(std::path::Path::new(root).join(inside.trim_start_matches("/work/project/"))),
            "{path}"
        );
    }
    // A project kept on a share gets no links at all.
    for share in ["//server/share/project", "\\\\server\\share\\project"] {
        let share = std::path::Path::new(share);
        assert_eq!(
            inside_project("src/app.rs", share, share),
            None,
            "{share:?}"
        );
    }
    // The project folder with its links followed counts as the project too.
    assert!(
        inside_project(
            "/private/work/project/a.rs",
            root,
            std::path::Path::new("/private/work/project")
        )
        .is_some()
    );
}

/// A link in a table cell opens nothing; its address stays in the cell,
/// after its words, and no longer turns up in the paragraph after the table.
#[test]
fn a_link_in_a_table_stays_in_its_cell() {
    let theme = crate::theme::Theme::editorial_dark();
    let lines = crate::markdown::render_markdown(
        "| file | docs |\n|---|---|\n| [app](src/app.rs:3) | [guide](https://example.com/guide) |\n\n\
         After the table.",
        100,
        &theme,
        "  ",
    );
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| span.opens.is_none())
    );
    let text = text_of(&lines);
    assert!(text.contains("app (src/app.rs:3)"), "{text}");
    assert!(text.contains("guide (https://example.com/guide)"), "{text}");
    assert!(
        text.lines().any(|row| row.trim() == "After the table."),
        "{text}"
    );
}

#[test]
fn a_reference_wrapped_over_two_rows_is_one_link() {
    let theme = crate::theme::Theme::editorial_dark();
    let path = format!("src/{}/deep.rs:3", "nested".repeat(4));
    let lines =
        crate::markdown::render_markdown(&format!("word word word `{path}`"), 24, &theme, "  ");
    let linked: Vec<_> = lines
        .iter()
        .filter_map(|line| line.spans.iter().find_map(|span| span.opens.clone()))
        .collect();
    assert!(linked.len() >= 2, "{lines:?}");
    assert!(
        linked
            .windows(2)
            .all(|pair| std::sync::Arc::ptr_eq(&pair[0], &pair[1]))
    );
}

fn text_of(lines: &[Line]) -> String {
    lines.iter().map(|line| line.text() + "\n").collect()
}

fn answer(text: &str, project: &Project) -> App {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage { text: text.into() },
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    local(&mut app, project);
    app
}

/// The frame drawn once the lookups the previous frame asked for are in.
fn settled_frame(app: &App, size: (u32, u32)) -> Vec<Line> {
    let _ = app.lines(size);
    app.reply_files.settle();
    app.lines(size)
}

fn file_regions(app: &App) -> Vec<crate::view::frame::HitRegion> {
    app.hit_rows
        .borrow()
        .iter()
        .filter(|hit| matches!(hit.target, crate::view::frame::HitTarget::ReplyFile(_)))
        .cloned()
        .collect()
}

fn cells_of(line: &Line, from: usize, to: usize) -> String {
    let mut column = 0;
    let mut out = String::new();
    for character in line.text().chars() {
        let width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if column >= from && column < to {
            out.push(character);
        }
        column += width;
    }
    out
}

/// Click targets cover the path itself, not the chip's padding, and only
/// paths to a project file get one. The first frame has none: it does not
/// wait for the disk.
#[test]
fn only_project_files_are_click_targets() {
    let project = Project::new();
    let text = "Open `src/app.rs:3`, not `src/missing.rs:9`.\n\n\
                Nor `src/outside.rs` or `src/lib`.\n\n\
                Read [the notes](docs/notes.md#L2).";
    let app = answer(text, &project);
    let size = (120, 30);
    let _ = app.lines(size);
    assert!(
        file_regions(&app).is_empty(),
        "the first frame waited for the disk"
    );
    let lines = settled_frame(&app, size);
    let regions = file_regions(&app);
    let named: Vec<String> = regions
        .iter()
        .map(|region| cells_of(&lines[region.row], region.start, region.end))
        .collect();
    assert_eq!(
        named,
        ["src/app.rs:3", "the notes", "docs/notes.md#L2"],
        "{regions:?}"
    );
    // With the pointer elsewhere the frame is identical to one without links.
    let mut remote = answer(text, &project);
    remote.files_local = false;
    assert_eq!(remote.lines(size), lines);
}

/// While an answer streams its paths are text; they open once it is final.
#[test]
fn a_streaming_answer_has_no_links_until_it_is_final() {
    let project = Project::new();
    let mut app = answer("", &project);
    app.items.clear();
    app.preview = "Look at `src/app.rs:3`".into();
    app.reasoning_preview = "Maybe `src/lib/util.rs`".into();
    app.thinking = crate::state::ThinkingMode::Show;
    let lines = settled_frame(&app, (120, 30));
    assert!(text_of(&lines).contains("src/app.rs:3"));
    assert!(file_regions(&app).is_empty());
    let finished = answer("Look at `src/app.rs:3`", &project);
    let _ = settled_frame(&finished, (120, 30));
    assert_eq!(file_regions(&finished).len(), 1);
}

/// A path waiting for a fresh answer keeps its link while the lookup thread
/// is idle or on a lookup that just started (a slow but working disk), and
/// loses it once a lookup has run for a second (a disk that stopped
/// answering).
#[test]
fn a_stuck_lookup_takes_the_link_away() {
    let project = Project::new();
    let app = answer("Open `src/app.rs:3`.", &project);
    let size = (120, 30);
    let _ = settled_frame(&app, size);
    let region = file_regions(&app).remove(0);
    let crate::view::frame::HitTarget::ReplyFile(link) = region.target else {
        unreachable!()
    };
    let now = std::time::Instant::now();
    let long_ago = now.checked_sub(std::time::Duration::from_secs(2)).unwrap();
    // Idle, or busy on a lookup that just started: the link stays, also for
    // a path queued long ago behind lookups that each answered quickly.
    for (asked, busy) in [
        (now, None),
        (now, Some(now)),
        (long_ago, None),
        (long_ago, Some(now)),
    ] {
        app.reply_files.stall(&link, asked, busy);
        let _ = app.lines(size);
        assert_eq!(file_regions(&app).len(), 1, "{asked:?} {busy:?}");
    }
    app.reply_files.stall(&link, now, Some(long_ago));
    let _ = app.lines(size);
    assert!(file_regions(&app).is_empty());
}

/// A project folder that is not there at the first lookup (a volume not
/// mounted yet) is looked for again: its links come once it is back.
#[test]
fn a_project_folder_missing_at_first_gets_its_links_once_it_is_back() {
    let project = Project::new();
    let away = project.folder.join("away");
    std::fs::rename(&project.root, &away).unwrap();
    let app = answer("Open `src/app.rs:3`.", &project);
    let size = (120, 30);
    let _ = settled_frame(&app, size);
    assert!(file_regions(&app).is_empty());
    std::fs::rename(&away, &project.root).unwrap();
    // The next look comes once the answer is stale and the last try is two
    // seconds old.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let _ = settled_frame(&app, size);
    let _ = settled_frame(&app, size);
    assert_eq!(file_regions(&app).len(), 1);
}

/// A click whose check the disk does not answer in time opens nothing, says
/// so, and is not passed on to what lies beneath (here, an opened thought).
#[tokio::test]
async fn a_click_the_disk_does_not_answer_in_time_says_so() {
    let project = Project::new();
    let thread = fixture_thread();
    let thought = message_item(
        1,
        &thread,
        ItemData::Reasoning {
            text: "Look at `src/app.rs:3`.".into(),
            duration_ms: Some(1200),
        },
    );
    let id = thought.id;
    let mut app = App::new(thread.clone(), vec![thread], vec![thought], vec![], vec![]);
    local(&mut app, &project);
    app.thinking = crate::state::ThinkingMode::Compact;
    app.expanded.insert(id);
    let _ = settled_frame(&app, (120, 30));
    let region = file_regions(&app).remove(0);
    let crate::view::frame::HitTarget::ReplyFile(link) = region.target.clone() else {
        unreachable!()
    };
    app.reply_files
        .slow_checks(std::time::Duration::from_secs(2));
    // A lookup found the file under two seconds ago: it opens at once,
    // without asking the slow disk again.
    let started = std::time::Instant::now();
    let ClickCheck::Open(file) = app.file_to_open(&link) else {
        panic!("a fresh answer did not open at once");
    };
    assert_eq!(file.path, project.file("src/app.rs"));
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
    // An older answer is checked again, and the click gives up after 300 ms.
    app.reply_files
        .age_answers(std::time::Duration::from_secs(3));
    let started = std::time::Instant::now();
    assert_eq!(
        app.file_to_open(&link),
        ClickCheck::Slow("src/app.rs".into())
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    let action = click_at(&mut app, region.start, region.row).await;
    assert!(matches!(action, crate::input::AppAction::Continue));
    assert_eq!(
        app.notice,
        "src/app.rs did not open: the disk was too slow to answer"
    );
    assert!(app.expanded.contains(&id), "the click folded the thought");
    app.reply_files.slow_checks(std::time::Duration::ZERO);
    assert!(matches!(app.file_to_open(&link), ClickCheck::Open(_)));
}

#[test]
fn no_file_opens_while_the_chat_is_on_another_computer() {
    let project = Project::new();
    let mut app = answer("Open `src/app.rs:3`.", &project);
    app.files_local = false;
    let _ = settled_frame(&app, (120, 30));
    assert!(file_regions(&app).is_empty());
    for (server, local) in [
        ("http://127.0.0.1:4096", true),
        ("http://localhost:4096", true),
        ("http://[::1]:4096", true),
        ("https://haarness.example", false),
        ("http://192.168.1.20:4096", false),
    ] {
        let client = crate::api::ApiClient::new(server, "t").unwrap();
        assert_eq!(client.files_on_this_computer(), local, "{server}");
    }
}

/// A chat with a folder of its own resolves paths in that folder.
#[test]
fn a_chat_with_its_own_folder_resolves_paths_there() {
    let project = Project::new();
    let mut app = answer("Open `lib/util.rs`.", &project);
    app.thread.directory = Some(project.root.join("src").display().to_string());
    let _ = settled_frame(&app, (120, 30));
    assert_eq!(file_regions(&app).len(), 1);
}

fn mouse(kind: MouseEventKind, column: usize, row: usize) -> MouseEvent {
    MouseEvent {
        kind,
        column: u16::try_from(column).unwrap(),
        row: u16::try_from(row).unwrap(),
        modifiers: KeyModifiers::NONE,
    }
}

/// Press at the first cell, move through the others, release at the last.
async fn press_through(app: &mut App, cells: &[(usize, usize)]) -> crate::input::AppAction {
    let client = offline_client();
    let (first, last) = (cells[0], cells[cells.len() - 1]);
    let left = MouseButton::Left;
    crate::input::mouse::handle_mouse(
        app,
        mouse(MouseEventKind::Down(left), first.0, first.1),
        &client,
    )
    .await
    .unwrap();
    for cell in &cells[1..] {
        crate::input::mouse::handle_mouse(
            app,
            mouse(MouseEventKind::Drag(left), cell.0, cell.1),
            &client,
        )
        .await
        .unwrap();
    }
    crate::input::mouse::handle_mouse(
        app,
        mouse(MouseEventKind::Up(left), last.0, last.1),
        &client,
    )
    .await
    .unwrap()
}

async fn click_at(app: &mut App, column: usize, row: usize) -> crate::input::AppAction {
    press_through(app, &[(column, row)]).await
}

#[tokio::test]
async fn a_click_opens_the_file_by_its_absolute_path_at_its_line() {
    let project = Project::new();
    let mut app = answer("Open `src/app.rs:3` or [this](./--cmd=x.rs).", &project);
    let _ = settled_frame(&app, (120, 30));
    let regions = file_regions(&app);
    let action = click_at(&mut app, regions[0].start + 2, regions[0].row).await;
    let crate::input::AppAction::OpenFile(file) = action else {
        panic!("the click opened nothing");
    };
    assert_eq!(file.path, project.file("src/app.rs"));
    assert_eq!(file.line, Some(3));
    // Relative to the project, in this system's separators.
    assert_eq!(
        std::path::Path::new(&file.shown),
        std::path::Path::new("src/app.rs")
    );
    // A file called like an editor option arrives as its full path.
    let action = click_at(&mut app, regions[1].start, regions[1].row).await;
    let crate::input::AppAction::OpenFile(file) = action else {
        panic!("the click opened nothing");
    };
    assert_eq!(file.path, project.file("--cmd=x.rs"));
    assert!(file.path.is_absolute());
}

/// A press that slips one cell on a path is a click and opens it; a longer
/// drag from it selects and copies, and opens nothing. So does a drag across
/// one.
#[tokio::test]
async fn a_drag_on_a_path_selects_and_a_slip_still_opens() {
    let project = Project::new();
    let mut app = answer("Open `src/app.rs:3` now.", &project);
    let _ = settled_frame(&app, (120, 30));
    let region = file_regions(&app).remove(0);
    let (start, row) = (region.start, region.row);
    let action = press_through(&mut app, &[(start, row), (start + 1, row)]).await;
    assert!(matches!(action, crate::input::AppAction::OpenFile(_)));
    let _ = app.lines((120, 30));
    let _ = crate::terminal::take_copied();
    let action = press_through(
        &mut app,
        &[(start, row), (start + 2, row), (start + 5, row)],
    )
    .await;
    assert!(matches!(action, crate::input::AppAction::Continue));
    assert_eq!(crate::terminal::take_copied(), ["src/ap"]);
    let _ = app.lines((120, 30));
    let action = press_through(
        &mut app,
        &[(start - 6, row), (start, row), (region.end + 3, row)],
    )
    .await;
    assert!(matches!(action, crate::input::AppAction::Continue));
    assert_eq!(crate::terminal::take_copied(), ["Open  src/app.rs:3  no"]);
}

/// In an opened thought a path opens its file and the thought stays open;
/// text that names no file folds the thought, as before.
#[tokio::test]
async fn a_path_in_an_opened_thought_opens_and_other_text_folds_it() {
    let project = Project::new();
    let thread = fixture_thread();
    let thought = message_item(
        1,
        &thread,
        ItemData::Reasoning {
            text: "Look at `src/app.rs:3` and `src/missing.rs`.".into(),
            duration_ms: Some(1200),
        },
    );
    let id = thought.id;
    let mut app = App::new(thread.clone(), vec![thread], vec![thought], vec![], vec![]);
    local(&mut app, &project);
    app.thinking = crate::state::ThinkingMode::Compact;
    app.expanded.insert(id);
    let lines = settled_frame(&app, (120, 30));
    let region = file_regions(&app).remove(0);
    let action = click_at(&mut app, region.start, region.row).await;
    assert!(matches!(action, crate::input::AppAction::OpenFile(_)));
    assert!(app.expanded.contains(&id), "the thought folded");
    let row = &lines[region.row].text();
    let missing = row.find("src/missing.rs").unwrap();
    let _ = app.lines((120, 30));
    let action = click_at(&mut app, missing, region.row).await;
    assert!(matches!(action, crate::input::AppAction::Continue));
    assert!(!app.expanded.contains(&id), "the thought stayed open");
}

/// A path removed since the frame was drawn opens nothing: the click goes to
/// whatever lies under it, here nothing.
#[tokio::test]
async fn a_click_on_a_file_since_removed_opens_nothing() {
    let project = Project::new();
    let mut app = answer("Open `src/lib/util.rs`.", &project);
    let _ = settled_frame(&app, (120, 30));
    let region = file_regions(&app).remove(0);
    std::fs::remove_file(project.root.join("src/lib/util.rs")).unwrap();
    // Past two seconds the click checks again rather than trust the lookup.
    app.reply_files
        .age_answers(std::time::Duration::from_secs(3));
    let action = click_at(&mut app, region.start, region.row).await;
    assert!(matches!(action, crate::input::AppAction::Continue));
}

/// Links on the way are read, never followed: one whose target is a share or
/// leaves the project is refused before the disk goes there, one that stays
/// inside is walked through, and a loop ends.
#[cfg(unix)]
#[test]
fn links_are_read_before_they_are_followed() {
    let project = Project::new();
    let root = &project.root;
    let resolved = dunce::canonicalize(root).unwrap();
    let link = |name: &str, target: &str| {
        std::os::unix::fs::symlink(target, root.join(name)).unwrap();
    };
    link("src/share", "//host/s");
    link("src/up", "../..");
    link("src/alias", "lib");
    link("src/away", &project.folder.display().to_string());
    link("src/loop", "loop");
    // Out of the project and back in: resolving it would end inside, but the
    // way goes through a place outside, which is never looked at.
    std::os::unix::fs::symlink(root.join("src/lib"), project.folder.join("back")).unwrap();
    link(
        "src/round",
        &project.folder.join("back").display().to_string(),
    );
    let walk = |path: &str| crate::reply_files::without_links(&root.join(path), root, &resolved);
    for path in [
        "src/share/a.rs",
        "src/up/outside.rs",
        "src/away/outside.rs",
        "src/loop/a.rs",
        "src/missing/a.rs",
        "src/round/util.rs",
    ] {
        assert_eq!(walk(path), None, "{path}");
    }
    let through = walk("src/alias/util.rs").unwrap();
    assert_eq!(
        dunce::canonicalize(&through).unwrap(),
        project.file("src/lib/util.rs")
    );
    // Below the project folder nothing on the way is a link any more.
    assert!(
        through
            .ancestors()
            .take_while(|step| *step != root.as_path() && *step != resolved.as_path())
            .all(|step| !step.symlink_metadata().unwrap().file_type().is_symlink()),
        "{through:?}"
    );
    let named = |path: &str| {
        project_file(
            root,
            &resolved,
            &Reference {
                path: path.into(),
                line: None,
            },
        )
    };
    assert_eq!(
        named("src/alias/util.rs"),
        Some(project.file("src/lib/util.rs"))
    );
    assert_eq!(named("src/share/a.rs"), None);
    assert_eq!(named("src/round/util.rs"), None);
}

/// A toast over a path takes the click there; a dialog takes every click.
#[tokio::test]
async fn a_toast_or_a_dialog_over_a_path_takes_the_click() {
    let project = Project::new();
    let row = format!("{} `src/app.rs:3`", "x".repeat(76));
    let text = vec![row.as_str(); 40].join("\n");
    let mut app = answer(&text, &project);
    let size = (100, 30);
    let _ = settled_frame(&app, size);
    let under = file_regions(&app)
        .into_iter()
        .find(|region| region.row == 3)
        .expect("a path on the toast's row");
    app.show_toast(
        None,
        "Saved".into(),
        crate::state::ToastVariant::Success,
        std::time::Duration::from_secs(30),
    );
    let _ = app.lines(size);
    assert_eq!(
        app.hit_at(under.row, under.start + 1),
        Some(crate::view::frame::HitTarget::ToastDismiss)
    );
    let action = click_at(&mut app, under.start + 1, under.row).await;
    assert!(matches!(action, crate::input::AppAction::Continue));
    assert!(app.notification_toast.is_none());
    app.overlay = crate::state::Overlay::Commands;
    let _ = app.lines(size);
    assert!(file_regions(&app).is_empty());
}

/// The path under the pointer is underlined in its own colours; another
/// mention of the same file is not.
#[test]
fn the_path_under_the_pointer_is_underlined_alone() {
    let project = Project::new();
    let mut app = answer("First `src/app.rs:3`.\n\nAgain `src/app.rs:3`.", &project);
    let size = (120, 30);
    let before = settled_frame(&app, size);
    let regions = file_regions(&app);
    assert_eq!(regions.len(), 2);
    let (first, second) = (&regions[0], &regions[1]);
    app.hovered_cell = Some((
        u16::try_from(first.start + 1).unwrap(),
        u16::try_from(first.row).unwrap(),
    ));
    let lines = app.lines(size);
    for column in first.start..first.end {
        let (fg, bg, attributes) = cell_style(&lines[first.row], column).unwrap();
        assert_ne!(attributes & attr::UNDERLINE, 0, "column {column}");
        let (drawn, _, _) = cell_style(&before[first.row], column).unwrap();
        assert_eq!(fg, drawn, "column {column}");
        assert_eq!(
            Some(bg),
            cell_style(&before[first.row], column).map(|cell| cell.1)
        );
    }
    // The padding and the other mention are as they were.
    let (_, _, attributes) = cell_style(&lines[first.row], first.start - 1).unwrap();
    assert_eq!(attributes & attr::UNDERLINE, 0);
    assert_eq!(lines[second.row], before[second.row]);
    assert_eq!(lines[first.row].text(), before[first.row].text());
}

/// A GUI editor that exits with an error is reported, by name, or as "your
/// text editor" for the system's own; one that succeeds is not.
#[cfg(unix)]
#[test]
fn a_gui_editor_that_failed_is_reported() {
    let project = Project::new();
    let mut app = answer("", &project);
    for (script, editor) in [
        ("exit 127", Some("gone")),
        ("exit 0", Some("fine")),
        ("exit 1", None),
    ] {
        let child = std::process::Command::new("sh")
            .args(["-c", script])
            .spawn()
            .unwrap();
        app.reply_files
            .started(child, editor.map(str::to_owned), "src/app.rs".into());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut failed = Vec::new();
    while failed.len() < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        failed.extend(app.reply_files.failed_editors());
    }
    // One that runs a while (an editor kept in the foreground) and then
    // exits with an error is the person's doing, and is not reported.
    let late = std::process::Command::new("sh")
        .args(["-c", "sleep 0.3; exit 1"])
        .spawn()
        .unwrap();
    app.reply_files
        .started(late, Some("gedit".into()), "src/app.rs".into());
    app.reply_files
        .launched_earlier(std::time::Duration::from_secs(60));
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(app.reply_files.failed_editors(), []);
    failed.sort();
    assert_eq!(
        failed,
        [
            (None, "src/app.rs".to_owned()),
            (Some("gone".to_owned()), "src/app.rs".to_owned())
        ]
    );
    assert_eq!(
        app.editor_failed(None, "src/app.rs"),
        "Your text editor could not open src/app.rs"
    );
    assert_eq!(
        app.editor_failed(Some("Notepad"), "src/app.rs"),
        "Notepad could not open src/app.rs"
    );
    app.language = crate::locale::Language::French;
    assert_ne!(
        app.editor_failed(None, "src/app.rs"),
        "Your text editor could not open src/app.rs"
    );
}

fn parts(launch: &Launch) -> (String, Vec<String>, bool) {
    (
        launch.program.to_string_lossy().into_owned(),
        launch
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        launch.gui,
    )
}

#[test]
fn each_editor_gets_the_file_at_its_line_and_none_is_waited_for() {
    let file = std::path::Path::new("/p/--cmd=a.rs");
    let shell = |editor: &str, gui: bool, location: &[&str]| {
        let mut args = vec![
            "-lc".to_owned(),
            format!("{editor} \"$@\""),
            "sh".to_owned(),
        ];
        args.extend(location.iter().map(|arg| (*arg).to_owned()));
        (String::from("sh"), args, gui)
    };
    for (editor, expected) in [
        ("vim", shell("vim", false, &["+42", "/p/--cmd=a.rs"])),
        (
            "nvim -u NONE",
            shell("nvim -u NONE", false, &["+42", "/p/--cmd=a.rs"]),
        ),
        (
            "emacsclient -t",
            shell("emacsclient -t", false, &["+42", "/p/--cmd=a.rs"]),
        ),
        ("nano", shell("nano", false, &["+42", "/p/--cmd=a.rs"])),
        ("hx", shell("hx", false, &["/p/--cmd=a.rs:42"])),
        (
            "code --wait",
            shell("code", true, &["-g", "/p/--cmd=a.rs:42"]),
        ),
        (
            "/usr/local/bin/cursor -w --new-window",
            shell(
                "/usr/local/bin/cursor --new-window",
                true,
                &["-g", "/p/--cmd=a.rs:42"],
            ),
        ),
        ("zed --wait", shell("zed", true, &["/p/--cmd=a.rs:42"])),
        (
            "'/opt/Sublime Text/subl' -w",
            shell("'/opt/Sublime Text/subl'", true, &["/p/--cmd=a.rs:42"]),
        ),
        (
            "mate -w",
            shell("mate", true, &["-l", "42", "/p/--cmd=a.rs"]),
        ),
        ("mvim -f", shell("mvim", true, &["+42", "/p/--cmd=a.rs"])),
        (
            "TERM=xterm-256color vim",
            shell("TERM=xterm-256color vim", false, &["+42", "/p/--cmd=a.rs"]),
        ),
        // An editor not listed gets the file alone.
        ("ed", shell("ed", false, &["/p/--cmd=a.rs"])),
    ] {
        assert_eq!(
            parts(&launch(Some(editor), "linux", file, Some(42))),
            expected,
            "{editor}"
        );
    }
    assert_eq!(
        parts(&launch(Some("code --wait"), "macos", file, None)),
        shell("code", true, &["/p/--cmd=a.rs"])
    );
    // Unset, or set to a system opener: a text editor, which never runs it.
    for editor in [
        None,
        Some(""),
        Some("open"),
        Some("xdg-open"),
        Some("/usr/bin/open -W"),
    ] {
        let mac = launch(editor, "macos", file, Some(42));
        assert_eq!(
            parts(&mac),
            (
                "open".to_owned(),
                vec!["-t".to_owned(), "/p/--cmd=a.rs".to_owned()],
                true
            ),
            "{editor:?}"
        );
        assert_eq!(mac.editor, None);
        assert_eq!(
            parts(&launch(editor, "linux", file, Some(42))),
            shell("vi", false, &["+42", "/p/--cmd=a.rs"]),
            "{editor:?}"
        );
        let windows = launch(editor, "windows", file, Some(42));
        assert_eq!(
            parts(&windows),
            (
                "notepad.exe".to_owned(),
                vec!["/p/--cmd=a.rs".to_owned()],
                true
            ),
            "{editor:?}"
        );
        assert_eq!(windows.editor.as_deref(), Some("Notepad"));
    }
    // Without a shell, on Windows, the words are run as written.
    assert_eq!(
        parts(&launch(Some("code --wait"), "windows", file, Some(7))),
        (
            "code".to_owned(),
            vec!["-g".to_owned(), "/p/--cmd=a.rs:7".to_owned()],
            true
        )
    );
    assert_eq!(
        parts(&launch(
            Some("\"C:\\Program Files\\Vim\\vim.exe\" -u NONE"),
            "windows",
            file,
            Some(7)
        )),
        (
            "C:\\Program Files\\Vim\\vim.exe".to_owned(),
            vec![
                "-u".to_owned(),
                "NONE".to_owned(),
                "+7".to_owned(),
                "/p/--cmd=a.rs".to_owned()
            ],
            false
        )
    );
}

/// The program lookup used on Windows never looks in a relative `PATH` entry,
/// the empty one included: they name the current folder, often the project.
#[test]
fn an_editor_is_never_found_in_a_relative_folder() {
    let project = Project::new();
    let bin = project.folder.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("code.cmd"), "").unwrap();
    let path = |folders: &[&std::path::Path]| std::env::join_paths(folders).unwrap();
    let relative = [
        std::path::Path::new(""),
        std::path::Path::new("."),
        std::path::Path::new("src"),
    ];
    // Tests run in the crate's folder, which has `src/lib.rs`.
    assert_eq!(
        crate::reply_files::find_program("lib".as_ref(), &path(&relative), &["rs"]),
        None
    );
    let mut folders = relative.to_vec();
    folders.push(&bin);
    assert_eq!(
        crate::reply_files::find_program("code".as_ref(), &path(&folders), &["exe", "cmd"]),
        Some(bin.join("code.cmd"))
    );
    assert_eq!(
        crate::reply_files::find_program("code.exe".as_ref(), &path(&folders), &["exe"]),
        None
    );
}

/// The editor gets the path as an argument of its own: a path full of shell
/// syntax reaches it unchanged and runs nothing.
#[cfg(unix)]
#[test]
fn the_shell_never_reads_the_path() {
    let project = Project::new();
    let record = project.folder.join("args");
    let editor = project.folder.join("record-args");
    std::fs::write(
        &editor,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
            record.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let file = project.root.join("$(touch pwned) `x`;a.rs");
    let launch = launch(Some(&editor.display().to_string()), "linux", &file, Some(5));
    assert!(launch.command().status().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(&record).unwrap(),
        format!("{}\n", file.display())
    );
    assert!(!project.root.join("pwned").exists());
}

/// A GUI editor runs in a process group of its own: Ctrl+Z in the TUI does
/// not stop it and closing the terminal does not end it.
#[cfg(unix)]
#[test]
fn a_gui_editor_gets_a_process_group_of_its_own() {
    let project = Project::new();
    let bin = project.folder.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let record = project.folder.join("group");
    let editor = bin.join("code");
    std::fs::write(
        &editor,
        format!("#!/bin/sh\nps -o pgid= -p $$ > '{}'\n", record.display()),
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let group = |pid: u32| {
        let output = std::process::Command::new("ps")
            .args(["-o", "pgid=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let gui = launch(
        Some(&editor.display().to_string()),
        "linux",
        &project.file("src/app.rs"),
        None,
    );
    assert!(gui.gui);
    assert!(gui.command().status().unwrap().success());
    let editor_group = std::fs::read_to_string(&record).unwrap().trim().to_owned();
    assert!(!editor_group.is_empty());
    assert_ne!(editor_group, group(std::process::id()));
}
