//! A glob pattern's `{a,b}` alternatives, as a shell expands them. The `glob`
//! crate reads none of this, so the glob tool walks each alternative, and its
//! permission check meets each of them.

use std::collections::HashSet;

/// The most patterns one pattern's `{a,b}` alternatives may stand for.
pub const MOST_ALTERNATIVES: usize = 32;

/// A pattern whose `{a,b}` alternatives stand for more than
/// [`MOST_ALTERNATIVES`] patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooManyAlternatives;

impl std::fmt::Display for TooManyAlternatives {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "its {{a,b}} alternatives stand for more than {MOST_ALTERNATIVES} patterns"
        )
    }
}

impl std::error::Error for TooManyAlternatives {}

/// The patterns `pattern` stands for once each `{a,b,…}` in it is replaced by
/// each of its alternatives, as a shell expands braces: `src/**/*.{ts,tsx}`
/// is `src/**/*.ts` and `src/**/*.tsx`, an alternative may hold more of them
/// (`{a,{b,c}}`), and the patterns come in that order, each once. Braces
/// with no comma between them (`{x}`, a template folder's `{{name}}`), a
/// `{` never closed or `}` never opened, and braces inside a `[…]` class
/// stand for themselves. `glob` reads none of this: each pattern that comes
/// back is for it to read on its own.
pub fn alternatives(pattern: &str) -> Result<Vec<String>, TooManyAlternatives> {
    let mut patterns = Vec::new();
    // Every group has two alternatives or more, so patterns that stand for
    // at most MOST_ALTERNATIVES are reached through fewer steps than this,
    // however many groups a pattern has.
    let mut steps = 2 * MOST_ALTERNATIVES;
    expand(pattern.to_owned(), 0, &mut patterns, &mut steps)?;
    let mut seen = HashSet::new();
    patterns.retain(|pattern| seen.insert(pattern.clone()));
    Ok(patterns)
}

/// Adds the patterns `pattern` stands for to `patterns`, its braces before
/// byte `from` already taken as written, in at most `steps` more steps.
fn expand(
    pattern: String,
    from: usize,
    patterns: &mut Vec<String>,
    steps: &mut usize,
) -> Result<(), TooManyAlternatives> {
    *steps = steps.checked_sub(1).ok_or(TooManyAlternatives)?;
    let Some((open, commas, close)) = brace_groups(&pattern)
        .into_iter()
        .find(|(open, _, _)| *open >= from)
    else {
        if patterns.len() == MOST_ALTERNATIVES {
            return Err(TooManyAlternatives);
        }
        patterns.push(pattern);
        return Ok(());
    };
    let mut start = open + 1;
    for end in commas.into_iter().chain([close]) {
        let next = format!(
            "{}{}{}",
            &pattern[..open],
            &pattern[start..end],
            &pattern[close + 1..]
        );
        expand(next, open, patterns, steps)?;
        start = end + 1;
    }
    Ok(())
}

/// Each `{…}` of `pattern` that holds a comma outside the braces nested in
/// it, in the order they open: where it opens, those commas, and where it
/// closes. A `[…]` class is passed over whole.
fn brace_groups(pattern: &str) -> Vec<(usize, Vec<usize>, usize)> {
    let bytes = pattern.as_bytes();
    let mut open: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut groups = Vec::new();
    // Once a `[` is never closed, no later one is.
    let mut classes = true;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'[' if classes => match class_end(bytes, at) {
                Some(end) => at = end,
                None => classes = false,
            },
            b'{' => open.push((at, Vec::new())),
            b',' => {
                if let Some((_, commas)) = open.last_mut() {
                    commas.push(at);
                }
            }
            b'}' => {
                if let Some((start, commas)) = open.pop()
                    && !commas.is_empty()
                {
                    groups.push((start, commas, at));
                }
            }
            _ => {}
        }
        at += 1;
    }
    groups.sort_by_key(|(start, _, _)| *start);
    groups
}

/// Where the `[…]` class opening at `open` ends, as `glob` reads one: a `]`
/// right after `[` or `[!` is in the class, not its end.
fn class_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut at = open + 1;
    if bytes.get(at) == Some(&b'!') {
        at += 1;
    }
    if bytes.get(at) == Some(&b']') {
        at += 1;
    }
    bytes
        .get(at..)?
        .iter()
        .position(|&byte| byte == b']')
        .map(|offset| at + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brace_alternatives_expand_as_a_shell_expands_them() {
        let expanded = |pattern: &str| alternatives(pattern).unwrap();
        assert_eq!(
            expanded("src/**/*.{ts,tsx}"),
            ["src/**/*.ts", "src/**/*.tsx"]
        );
        assert_eq!(
            expanded("{src,lib}/{a,{b,c}}.rs"),
            [
                "src/a.rs", "src/b.rs", "src/c.rs", "lib/a.rs", "lib/b.rs", "lib/c.rs"
            ]
        );
        // An empty alternative, and the same pattern twice.
        assert_eq!(expanded("*.{rs,}"), ["*.rs", "*."]);
        assert_eq!(expanded("{a,a}/x"), ["a/x"]);
        // As written: no comma, never closed, never opened, in a class.
        for pattern in [
            "{x}",
            "{{name}}/*.py",
            "a{b",
            "a}b",
            "[{,}]x",
            "[]{,}]x",
            "[!]{,}]x",
            "plain",
            "",
        ] {
            assert_eq!(expanded(pattern), [pattern], "{pattern}");
        }
        // What a shell makes of braces some of which are taken as written.
        assert_eq!(expanded("{a{b,c}"), ["{ab", "{ac"]);
        assert_eq!(expanded("{x{a,b}}"), ["{xa}", "{xb}"]);
        assert_eq!(expanded("[x{a,b}"), ["[xa", "[xb"]);
        // 32 patterns and no more, however many groups ask for more.
        assert_eq!(expanded("{a,b}{c,d}{e,f}{g,h}{i,j}").len(), 32);
        assert_eq!(
            alternatives("{a,b}{c,d}{e,f}{g,h}{i,j}{k,l}"),
            Err(TooManyAlternatives)
        );
        let deep = "{a,b}".repeat(10_000);
        assert_eq!(alternatives(&deep), Err(TooManyAlternatives));
        assert_eq!(
            alternatives(&format!("{{{}}}", ",x".repeat(40))),
            Err(TooManyAlternatives)
        );
    }
}
