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
//! Parsing is pure; applying the operations is an adapter concern.

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

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
                    });
                    continue;
                }
                let hunk = match hunks.last_mut() {
                    Some(hunk) => hunk,
                    None => {
                        hunks.push(Hunk {
                            context_header: None,
                            lines: Vec::new(),
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
        } else if trimmed.trim().is_empty() {
            // Blank lines between operations are tolerated.
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

/// Apply parsed hunks to file content. Pure so it is testable without a
/// filesystem: returns the new content.
pub fn apply_hunks(content: &str, hunks: &[Hunk]) -> Result<String, String> {
    let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();
    let ends_with_newline = content.is_empty() || content.ends_with('\n');
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
        let new: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                HunkLine::Context(text) | HunkLine::Add(text) => Some(text.clone()),
                HunkLine::Remove(_) => None,
            })
            .collect();
        let search_from = match &hunk.context_header {
            Some(header) => {
                let header = header.trim();
                lines[cursor..]
                    .iter()
                    .position(|line| line.contains(header))
                    .map_or(cursor, |offset| cursor + offset)
            }
            None => cursor,
        };
        let matches: Vec<usize> = (search_from..=lines.len().saturating_sub(old.len()))
            .filter(|&start| {
                old.iter().enumerate().all(|(offset, expected)| {
                    lines
                        .get(start + offset)
                        .is_some_and(|actual| actual.trim_end() == expected.trim_end())
                })
            })
            .collect();
        match matches.as_slice() {
            [] => {
                return Err(format!(
                    "hunk context not found: {:?}",
                    old.first().copied().unwrap_or_default()
                ));
            }
            [start] => {
                let replaced: Vec<String> = new;
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
    let mut result = lines.join("\n");
    if ends_with_newline && !result.is_empty() {
        result.push('\n');
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{FileOp, HunkLine, apply_hunks, parse_patch};

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
}
