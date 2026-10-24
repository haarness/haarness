//! Parser for the V4A patch envelope GPT-family models emit:
//!
//! ```text
//! *** Begin Patch
//! *** Add File: docs/new.md
//! +content line
//! *** Update File: src/lib.rs
//! *** Move to: src/renamed.rs
//! @@ fn nearby_context()
//!  context
//! -removed
//! +added
//! *** Delete File: obsolete.txt
//! *** End Patch
//! ```
//!
//! A `*** End of File` line after a hunk (the Codex spelling) anchors it to
//! the end of the file. Parsing is pure; applying the operations is an
//! adapter concern.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Add {
        path: String,
        content: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<Hunk>,
    },
    Delete {
        path: String,
    },
}

impl FileOp {
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Add { path, .. } | Self::Update { path, .. } | Self::Delete { path } => path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// Optional `@@ <text>` locator narrowing where the hunk applies.
    pub context_header: Option<String>,
    pub lines: Vec<HunkLine>,
    /// `*** End of File` closed the hunk: its old lines end the file, and
    /// lines it only adds go at the end.
    pub end_of_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

const END_OF_FILE: &str = "*** End of File";

pub fn parse_patch(input: &str) -> Result<Vec<FileOp>, String> {
    let mut lines = input.trim_start().lines().peekable();
    // The header carries no information, so a stuttered marker such as
    // `*** Begin Patch Patch` (seen from real models) is not worth a retry.
    match lines.next().map(str::trim) {
        Some(header) if header.starts_with("*** Begin Patch") => {}
        Some(other) if !other.is_empty() => {
            return Err(format!(
                "patch must start with `*** Begin Patch`; the first line was `{}`",
                other.chars().take(80).collect::<String>()
            ));
        }
        _ => return Err("patch is empty; start it with `*** Begin Patch`".into()),
    }
    let mut operations = Vec::new();
    let mut ended = false;
    while let Some(line) = lines.next() {
        let trimmed = line.trim_end();
        if trimmed.trim() == "*** End Patch" {
            ended = true;
            break;
        }
        if let Some(path) = trimmed.strip_prefix("*** Add File: ") {
            let mut content = String::new();
            while let Some(next) = lines.peek() {
                if next.starts_with("***") {
                    break;
                }
                let row = lines.next().unwrap_or_default();
                let Some(added) = row.strip_prefix('+') else {
                    return Err(format!("added files only take + lines, got {row:?}"));
                };
                content.push_str(added);
                content.push('\n');
            }
            operations.push(FileOp::Add {
                path: path.trim().to_owned(),
                content,
            });
        } else if let Some(path) = trimmed.strip_prefix("*** Update File: ") {
            let mut move_to = None;
            if let Some(next) = lines.peek() {
                if let Some(target) = next.trim_end().strip_prefix("*** Move to: ") {
                    move_to = Some(target.trim().to_owned());
                    lines.next();
                }
            }
            let mut hunks: Vec<Hunk> = Vec::new();
            while let Some(next) = lines.peek() {
                let peeked = next.trim_end();
                if peeked == END_OF_FILE {
                    lines.next();
                    match hunks.last_mut() {
                        Some(hunk) if !hunk.end_of_file => hunk.end_of_file = true,
                        _ => hunks.push(Hunk {
                            context_header: None,
                            lines: Vec::new(),
                            end_of_file: true,
                        }),
                    }
                    continue;
                }
                if peeked.starts_with("*** ") || peeked.trim() == "*** End Patch" {
                    break;
                }
                let row = lines.next().unwrap_or_default();
                let row = row.strip_suffix('\r').unwrap_or(row);
                if let Some(header) = row.strip_prefix("@@") {
                    hunks.push(Hunk {
                        context_header: {
                            let header = header.trim();
                            (!header.is_empty()).then(|| header.to_owned())
                        },
                        lines: Vec::new(),
                        end_of_file: false,
                    });
                    continue;
                }
                // A line after `*** End of File` starts a hunk of its own.
                let hunk = match hunks.last_mut() {
                    Some(hunk) if !hunk.end_of_file => hunk,
                    _ => {
                        hunks.push(Hunk {
                            context_header: None,
                            lines: Vec::new(),
                            end_of_file: false,
                        });
                        hunks.last_mut().unwrap()
                    }
                };
                if let Some(added) = row.strip_prefix('+') {
                    hunk.lines.push(HunkLine::Add(added.to_owned()));
                } else if let Some(removed) = row.strip_prefix('-') {
                    hunk.lines.push(HunkLine::Remove(removed.to_owned()));
                } else if let Some(context) = row.strip_prefix(' ') {
                    hunk.lines.push(HunkLine::Context(context.to_owned()));
                } else if row.is_empty() {
                    hunk.lines.push(HunkLine::Context(String::new()));
                } else {
                    return Err(format!(
                        "hunk lines must start with ' ', '+' or '-': {row:?}"
                    ));
                }
            }
            if hunks.iter().all(|hunk| hunk.lines.is_empty()) {
                return Err(format!("update for {path} contains no hunk lines"));
            }
            operations.push(FileOp::Update {
                path: path.trim().to_owned(),
                move_to,
                hunks,
            });
        } else if let Some(path) = trimmed.strip_prefix("*** Delete File: ") {
            operations.push(FileOp::Delete {
                path: path.trim().to_owned(),
            });
        } else if trimmed.trim().is_empty() || trimmed == END_OF_FILE {
            // Blank lines between operations are tolerated, and so is an
            // end-of-file marker after an added file's lines.
        } else {
            return Err(format!("unrecognized patch directive: {trimmed:?}"));
        }
    }
    if !ended {
        return Err("patch must end with *** End Patch".into());
    }
    if operations.is_empty() {
        return Err("patch contains no file operations".into());
    }
    Ok(operations)
}

/// One line of a file: its text and the line break that ends it (`""` for
/// a last line without one).
struct Line {
    text: String,
    ending: &'static str,
}

/// The first line from `from` that an `@@` locator names: one that is the
/// locator once trimmed, else one that contains it.
fn locate(lines: &[Line], from: usize, header: &str) -> Option<usize> {
    let lines = lines.get(from..)?;
    lines
        .iter()
        .position(|line| line.text.trim() == header)
        .or_else(|| lines.iter().position(|line| line.text.contains(header)))
        .map(|offset| from + offset)
}

/// Apply parsed hunks to file content. Pure so it is testable without a
/// filesystem: returns the new content.
///
/// Every line keeps its own line break, so a CRLF file stays CRLF; the
/// lines a hunk adds take the break most of the file's lines end with.
pub fn apply_hunks(content: &str, hunks: &[Hunk]) -> Result<String, String> {
    let mut lines: Vec<Line> = content
        .split_inclusive('\n')
        .map(|line| {
            let (text, ending) = if let Some(text) = line.strip_suffix("\r\n") {
                (text, "\r\n")
            } else if let Some(text) = line.strip_suffix('\n') {
                (text, "\n")
            } else {
                (line, "")
            };
            Line {
                text: text.to_owned(),
                ending,
            }
        })
        .collect();
    let ends_with_newline = content.is_empty() || content.ends_with('\n');
    let crlf = lines.iter().filter(|line| line.ending == "\r\n").count();
    let lf = lines.iter().filter(|line| line.ending == "\n").count();
    let newline = if crlf > lf { "\r\n" } else { "\n" };
    // A last line without a break gets one while lines may follow it; the
    // result ends without one again below.
    if let Some(last) = lines.last_mut()
        && last.ending.is_empty()
    {
        last.ending = newline;
    }
    // Lines a hunk only adds go at the end, after every other hunk.
    let mut appended = Vec::new();
    let mut cursor = 0usize;
    for hunk in hunks {
        let old: Vec<&str> = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                HunkLine::Context(text) | HunkLine::Remove(text) => Some(text.as_str()),
                HunkLine::Add(_) => None,
            })
            .collect();
        let header = hunk.context_header.as_deref().map(str::trim);
        if old.is_empty() {
            // As in Codex, whatever its locator says. A locator still moves
            // where the next hunk looks, so `@@ class` then `@@ fn` narrows
            // the hunk after them.
            if let Some(found) = header.and_then(|header| locate(&lines, cursor, header)) {
                cursor = found + 1;
            }
            appended.extend(hunk.lines.iter().filter_map(|line| match line {
                HunkLine::Add(text) => Some(Line {
                    text: text.clone(),
                    ending: newline,
                }),
                HunkLine::Context(_) | HunkLine::Remove(_) => None,
            }));
            continue;
        }
        // Where the hunk is looked for: from the line its locator names, then
        // (as before) from the first line that contains the locator, else
        // from the cursor.
        let mut froms = Vec::new();
        if let Some(header) = header {
            let within = (cursor..lines.len()).find(|&index| lines[index].text.contains(header));
            froms.extend(locate(&lines, cursor, header));
            froms.extend(within.filter(|index| !froms.contains(index)));
        }
        if froms.is_empty() {
            froms.push(cursor);
        }
        let last = lines.len().saturating_sub(old.len());
        let matches = froms
            .into_iter()
            .map(|search_from| {
                // An end-of-file hunk is tried only where its old lines end
                // the file.
                let first = if hunk.end_of_file {
                    last.max(search_from)
                } else {
                    search_from
                };
                (first..=last)
                    .filter(|&start| {
                        old.iter().enumerate().all(|(offset, expected)| {
                            lines
                                .get(start + offset)
                                .is_some_and(|actual| actual.text.trim_end() == expected.trim_end())
                        })
                    })
                    .collect::<Vec<usize>>()
            })
            .find(|matches| !matches.is_empty())
            .unwrap_or_default();
        match matches.as_slice() {
            [] if hunk.end_of_file => {
                return Err(format!(
                    "hunk context not found at the end of the file: {:?}",
                    old.first().copied().unwrap_or_default()
                ));
            }
            [] => {
                return Err(format!(
                    "hunk context not found: {:?}",
                    old.first().copied().unwrap_or_default()
                ));
            }
            [start] => {
                // Context lines keep the break of the line they matched.
                let mut endings = lines[*start..*start + old.len()]
                    .iter()
                    .map(|line| line.ending);
                let replaced: Vec<Line> = hunk
                    .lines
                    .iter()
                    .filter_map(|line| match line {
                        HunkLine::Context(text) => Some(Line {
                            text: text.clone(),
                            ending: endings.next().unwrap_or(newline),
                        }),
                        HunkLine::Remove(_) => {
                            endings.next();
                            None
                        }
                        HunkLine::Add(text) => Some(Line {
                            text: text.clone(),
                            ending: newline,
                        }),
                    })
                    .collect();
                let count = replaced.len();
                lines.splice(*start..*start + old.len(), replaced);
                cursor = start + count;
            }
            _ => {
                return Err(format!(
                    "hunk context is ambiguous ({} matches): {:?}; retry with a unique @@ locator and more unchanged context lines",
                    matches.len(),
                    old.first().copied().unwrap_or_default()
                ));
            }
        }
    }
    lines.extend(appended);
    if !ends_with_newline && let Some(last) = lines.last_mut() {
        last.ending = "";
    }
    let mut result = String::with_capacity(content.len());
    for line in &lines {
        result.push_str(&line.text);
        result.push_str(line.ending);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{FileOp, Hunk, HunkLine, apply_hunks, parse_patch};

    /// The hunks of a patch's first (update) operation.
    fn update_hunks(patch: &str) -> Vec<Hunk> {
        match parse_patch(patch).unwrap().into_iter().next().unwrap() {
            FileOp::Update { hunks, .. } => hunks,
            _ => panic!("expected an update"),
        }
    }

    /// `body` applied to `content` as the hunks of one update.
    fn patched(content: &str, body: &str) -> Result<String, String> {
        let patch = format!("*** Begin Patch\n*** Update File: f\n{body}*** End Patch\n");
        apply_hunks(content, &update_hunks(&patch))
    }

    #[test]
    fn parses_add_update_move_and_delete() {
        let patch = "*** Begin Patch\n\
*** Add File: docs/new.md\n\
+hello\n\
+world\n\
*** Update File: src/lib.rs\n\
*** Move to: src/renamed.rs\n\
@@ fn main\n \
before\n\
-old\n\
+new\n \
after\n\
*** Delete File: gone.txt\n\
*** End Patch\n";
        let operations = parse_patch(patch).unwrap();
        assert_eq!(operations.len(), 3);
        assert!(matches!(&operations[0], FileOp::Add { path, content }
            if path == "docs/new.md" && content == "hello\nworld\n"));
        let FileOp::Update {
            path,
            move_to,
            hunks,
        } = &operations[1]
        else {
            panic!("expected update");
        };
        assert_eq!(path, "src/lib.rs");
        assert_eq!(move_to.as_deref(), Some("src/renamed.rs"));
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].context_header.as_deref(), Some("fn main"));
        assert!(matches!(&hunks[0].lines[1], HunkLine::Remove(text) if text == "old"));
        assert!(matches!(&operations[2], FileOp::Delete { path } if path == "gone.txt"));
    }

    #[test]
    fn rejects_malformed_envelopes() {
        assert!(parse_patch("no envelope").is_err());
        assert!(parse_patch("*** Begin Patch\n*** Update File: x\n*** End Patch\n").is_err());
        assert!(parse_patch("*** Begin Patch\n*** Add File: x\n+a\n").is_err());
        assert!(
            parse_patch("*** Begin Patch\n*** Update File: x\n?bogus\n*** End Patch\n").is_err()
        );
        let error = parse_patch("--- a/x\n+++ b/x\n").unwrap_err();
        assert_eq!(
            error,
            "patch must start with `*** Begin Patch`; the first line was `--- a/x`"
        );
        assert!(parse_patch("  \n").unwrap_err().contains("patch is empty"));
    }

    #[test]
    fn tolerates_a_stuttered_begin_marker() {
        let operations =
            parse_patch("\n*** Begin Patch Patch\n*** Delete File: gone.txt\n*** End Patch\n")
                .unwrap();
        assert!(matches!(&operations[0], FileOp::Delete { path } if path == "gone.txt"));
    }

    #[test]
    fn applies_multi_hunk_updates_in_order() {
        let content = "one\ntwo\nthree\nfour\nfive\nsix\n";
        let patch = "*** Begin Patch\n\
*** Update File: f\n\
@@\n \
one\n\
-two\n\
+TWO\n\
@@\n \
five\n\
-six\n\
+SIX\n\
*** End Patch\n";
        let operations = parse_patch(patch).unwrap();
        let FileOp::Update { hunks, .. } = &operations[0] else {
            panic!()
        };
        let updated = apply_hunks(content, hunks).unwrap();
        assert_eq!(updated, "one\nTWO\nthree\nfour\nfive\nSIX\n");
    }

    #[test]
    fn context_mismatch_and_ambiguity_are_errors() {
        let hunks = |patch: &str| {
            let operations = parse_patch(patch).unwrap();
            match operations.into_iter().next().unwrap() {
                FileOp::Update { hunks, .. } => hunks,
                _ => panic!(),
            }
        };
        let missing = hunks("*** Begin Patch\n*** Update File: f\n-absent\n+x\n*** End Patch\n");
        assert!(
            apply_hunks("a\nb\n", &missing)
                .unwrap_err()
                .contains("not found")
        );

        let ambiguous = hunks("*** Begin Patch\n*** Update File: f\n-dup\n+x\n*** End Patch\n");
        assert!(
            apply_hunks("dup\nmid\ndup\n", &ambiguous)
                .unwrap_err()
                .contains("ambiguous")
        );
    }

    #[test]
    fn trailing_whitespace_differences_are_tolerated() {
        let hunks = match parse_patch(
            "*** Begin Patch\n*** Update File: f\n-keep me  \n+kept\n*** End Patch\n",
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        {
            FileOp::Update { hunks, .. } => hunks,
            _ => panic!(),
        };
        let updated = apply_hunks("keep me\n", &hunks).unwrap();
        assert_eq!(updated, "kept\n");
    }

    #[test]
    fn keeps_each_lines_break_and_adds_lines_with_the_files_own() {
        let body = "@@\n one\n-two\n+TWO\n+added\n three\n";
        assert_eq!(
            patched("one\r\ntwo\r\nthree\r\nfour\r\n", body).unwrap(),
            "one\r\nTWO\r\nadded\r\nthree\r\nfour\r\n"
        );
        // A patch written with CRLF reads the same.
        assert_eq!(
            patched(
                "one\r\ntwo\r\nthree\r\nfour\r\n",
                &body.replace('\n', "\r\n")
            )
            .unwrap(),
            "one\r\nTWO\r\nadded\r\nthree\r\nfour\r\n"
        );
        // In a mixed file each line keeps its own break, a context line
        // included, and added lines take the one most lines end with.
        assert_eq!(
            patched("one\ntwo\nthree\r\nfour\n", body).unwrap(),
            "one\nTWO\nadded\nthree\r\nfour\n"
        );
        assert_eq!(
            patched("one\r\ntwo\r\nthree\nfour\r\n", body).unwrap(),
            "one\r\nTWO\r\nadded\r\nthree\nfour\r\n"
        );
        // A file without a final line break still ends without one.
        assert_eq!(
            patched("one\r\ntwo\r\nthree", "@@\n-three\n+3\n").unwrap(),
            "one\r\ntwo\r\n3"
        );
        assert_eq!(
            patched("one\r\ntwo", "@@\n+three\n").unwrap(),
            "one\r\ntwo\r\nthree"
        );
        assert_eq!(patched("one\ntwo", "@@\n-two\n").unwrap(), "one");
    }

    #[test]
    fn end_of_file_marker_anchors_its_hunk_to_the_end() {
        let patch = concat!(
            "*** Begin Patch\n*** Update File: f\n@@\n-x\n+y\n*** End of File\n",
            "@@ second\n-a\n+b\n*** Add File: g\n+g\n*** End of File\n*** End Patch\n"
        );
        let operations = parse_patch(patch).unwrap();
        assert_eq!(operations.len(), 2);
        let FileOp::Update { hunks, .. } = &operations[0] else {
            panic!("expected update");
        };
        assert!(hunks[0].end_of_file);
        assert!(!hunks[1].end_of_file);
        assert!(matches!(&operations[1], FileOp::Add { content, .. } if content == "g\n"));

        // Only the copy that ends the file matches.
        let eof = "@@\n-x\n+y\n*** End of File\n";
        assert_eq!(patched("x\nmid\nx\n", eof).unwrap(), "x\nmid\ny\n");
        let error = patched("x\nend\n", eof).unwrap_err();
        assert!(error.contains("at the end of the file"), "{error}");
        // Lines after the marker start another hunk.
        assert_eq!(
            patched("a\nx\n", "@@\n-x\n+y\n*** End of File\n-a\n+b\n").unwrap_err(),
            "hunk context not found: \"a\""
        );
    }

    #[test]
    fn pure_additions_append_at_the_end_whatever_their_locator() {
        assert_eq!(patched("a\nb\n", "@@\n+c\n+d\n").unwrap(), "a\nb\nc\nd\n");
        assert_eq!(
            patched("a\nb\n", "@@\n+c\n*** End of File\n").unwrap(),
            "a\nb\nc\n"
        );
        assert_eq!(patched("", "@@\n+first\n").unwrap(), "first\n");
        assert_eq!(patched("a", "@@\n+b\n").unwrap(), "a\nb");
        // An addition goes at the end after the other hunks, which still
        // match from where they would have.
        assert_eq!(
            patched("one\ntwo\nthree\n", "@@\n+after\n@@\n one\n-two\n+TWO\n").unwrap(),
            "one\nTWO\nthree\nafter\n"
        );
        // A locator, found once, twice or not at all, changes nothing about
        // where (as in Codex), so it never lands inside a function.
        let file = "def a():\n    return 1\n";
        let added = "\ndef b():\n    return 2\n";
        for locator in ["def a():", "return", "missing"] {
            assert_eq!(
                patched(
                    file,
                    &format!("@@ {locator}\n+\n+def b():\n+    return 2\n")
                )
                .unwrap(),
                format!("{file}{added}"),
                "{locator}"
            );
        }
        assert_eq!(patched("", "@@ top\n+x = 1\n").unwrap(), "x = 1\n");
        // An edit and then an addition under the same locator.
        assert_eq!(
            patched(
                file,
                "@@ def a():\n-    return 1\n+    return 10\n@@ def a():\n+\n+def b():\n+    return 2\n"
            )
            .unwrap(),
            "def a():\n    return 10\n\ndef b():\n    return 2\n"
        );
        // A locator alone narrows the hunk after it (`@@ class` then `@@ fn`).
        assert_eq!(
            patched(
                "impl B {\n    fn run() {\n        old();\n    }\n}\nimpl A {\n    fn run() {\n        old();\n    }\n}\n",
                "@@ impl A\n@@ fn run\n-        old();\n+        new();\n"
            )
            .unwrap(),
            "impl B {\n    fn run() {\n        old();\n    }\n}\nimpl A {\n    fn run() {\n        new();\n    }\n}\n"
        );
    }

    #[test]
    fn a_locator_names_the_line_it_is_before_one_that_mentions_it() {
        // `// see fn run()` mentions the locator, and a hunk looked for after
        // it would be ambiguous; the hunk is under the line that is it.
        assert_eq!(
            patched(
                "// see fn run()\n    step();\nfn run()\n    step();\n",
                "@@ fn run()\n-    step();\n+    STEP();\n"
            )
            .unwrap(),
            "// see fn run()\n    step();\nfn run()\n    STEP();\n"
        );
        let file = "// see fn run()\nfn run()\n    step();\n";
        // As before, a hunk found only after a line that contains the
        // locator still applies there.
        assert_eq!(
            patched(file, "@@ fn run()\n-// see fn run()\n+// see run()\n").unwrap(),
            "// see run()\nfn run()\n    step();\n"
        );
        assert_eq!(
            patched(
                "// call fn run\nx = 1\nfn run\n",
                "@@ fn run\n-x = 1\n+x = 2\n"
            )
            .unwrap(),
            "// call fn run\nx = 2\nfn run\n"
        );
    }
}
