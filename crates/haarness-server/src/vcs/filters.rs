//! The clean, smudge and process filters the review of an untrusted project
//! empties.
//!
//! A repository's own configuration can name a program as a filter that its
//! `.gitattributes` selects, and Git runs it whenever a diff reads a changed
//! file from the working tree. A folder someone else prepared (a copied
//! checkout, an unpacked archive) brings that configuration with it, so when
//! project configuration is not trusted the review empties every filter the
//! repository itself defines before Git reads anything. Filters from the
//! user's own global and system configuration still run.

use std::{collections::BTreeSet, ffi::OsString};

/// The commands `git lfs install` writes for its filter. They run the user's
/// installed git-lfs, so they are kept.
const LFS: [&[u8]; 5] = [
    b"git-lfs clean -- %f",
    b"git-lfs smudge -- %f",
    b"git-lfs smudge --skip -- %f",
    b"git-lfs filter-process",
    b"git-lfs filter-process --skip",
];

/// Why the filters could not be emptied, so Git is not run.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// A filter's name is not UTF-8, so no override can name it.
    NotUtf8,
    /// Git before 2.31 takes overrides only as `-c`, which cannot name a
    /// filter whose name holds `=`.
    NeedsNewerGit,
}

/// One entry of `git config -z --list`, with its scope when the listing was
/// made with `--show-scope`. Read as bytes: a filter's name need not be UTF-8.
struct Entry<'a> {
    scope: Option<&'a [u8]>,
    key: &'a [u8],
    value: &'a [u8],
}

/// The entries of a `-z --list` listing. With `scoped`, each entry follows
/// its scope (`--show-scope`); otherwise every entry counts as the
/// repository's own.
fn entries(listing: &[u8], scoped: bool) -> Vec<Entry<'_>> {
    let mut fields = listing
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut entries = Vec::new();
    loop {
        let scope = if scoped {
            match fields.next() {
                Some(scope) => Some(scope),
                None => break,
            }
        } else {
            None
        };
        let Some(entry) = fields.next() else {
            break;
        };
        let (key, value) = match entry.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&entry[..at], &entry[at + 1..]),
            None => (entry, &entry[entry.len()..]),
        };
        entries.push(Entry { scope, key, value });
    }
    entries
}

/// The filter a `filter.<name>.clean|smudge|process` key configures. The
/// name may be empty: `[filter ""]` is selected by `filter=` in
/// `.gitattributes`.
fn filter_name(key: &[u8]) -> Option<&[u8]> {
    let rest = key.strip_prefix(b"filter.")?;
    [&b".clean"[..], b".smudge", b".process"]
        .iter()
        .find_map(|kind| rest.strip_suffix(*kind))
}

/// The settings that empty every filter the repository's own configuration
/// defines, apart from the standard git-lfs commands, given a `-z --list`
/// listing (`scoped` when made with `--show-scope`). When that configuration
/// defines a git-lfs extension, git-lfs runs the program it names, so every
/// filter is emptied, git-lfs's and those of every scope included. Each
/// emptied filter is also made optional, so Git reads the file as it is.
pub(super) fn overrides(listing: &[u8], scoped: bool) -> Result<Vec<(String, String)>, Refusal> {
    let entries = entries(listing, scoped);
    let own = |entry: &Entry<'_>| {
        entry
            .scope
            .is_none_or(|scope| matches!(scope, b"local" | b"worktree"))
    };
    let extension = entries.iter().filter(|entry| own(entry)).any(|entry| {
        entry.key.starts_with(b"lfs.extension.")
            && (entry.key.ends_with(b".clean") || entry.key.ends_with(b".smudge"))
    });
    let mut keys = BTreeSet::new();
    let mut names = BTreeSet::new();
    for entry in &entries {
        let Some(name) = filter_name(entry.key) else {
            continue;
        };
        if extension || (own(entry) && !LFS.contains(&entry.value)) {
            let (Ok(key), Ok(name)) = (std::str::from_utf8(entry.key), std::str::from_utf8(name))
            else {
                return Err(Refusal::NotUtf8);
            };
            keys.insert(key.to_owned());
            names.insert(name.to_owned());
        }
    }
    Ok(keys
        .into_iter()
        .map(|key| (key, String::new()))
        .chain(
            names
                .into_iter()
                .map(|name| (format!("filter.{name}.required"), "false".to_owned())),
        )
        .collect())
}

/// Git's version from `git version` (`git version 2.39.5 (Apple Git-154)`,
/// `git version 2.45.1.windows.1`).
pub(super) fn version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.trim().strip_prefix("git version ")?.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts
        .next()?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor))
}

/// How `pairs` reach Git: as `GIT_CONFIG_*` variables after the `base` pairs
/// already in the environment (Git 2.31 and later, any key), or else as
/// `-c key=value` arguments, which cannot name a key holding `=`. An unknown
/// version is taken as an old one.
pub(super) fn carry(
    pairs: &[(String, String)],
    version: Option<(u32, u32)>,
    base: usize,
) -> Result<(Vec<OsString>, Vec<(String, String)>), Refusal> {
    if pairs.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    if version.is_some_and(|version| version >= (2, 31)) {
        let mut variables = Vec::with_capacity(pairs.len() * 2 + 1);
        for (index, (key, value)) in pairs.iter().enumerate() {
            variables.push((format!("GIT_CONFIG_KEY_{}", base + index), key.clone()));
            variables.push((format!("GIT_CONFIG_VALUE_{}", base + index), value.clone()));
        }
        variables.push((
            "GIT_CONFIG_COUNT".to_owned(),
            (base + pairs.len()).to_string(),
        ));
        return Ok((Vec::new(), variables));
    }
    if pairs.iter().any(|(key, _)| key.contains('=')) {
        return Err(Refusal::NeedsNewerGit);
    }
    Ok((
        pairs
            .iter()
            .flat_map(|(key, value)| {
                [
                    OsString::from("-c"),
                    OsString::from(format!("{key}={value}")),
                ]
            })
            .collect(),
        Vec::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scoped(entries: &[(&str, &str, &str)]) -> Vec<u8> {
        entries
            .iter()
            .flat_map(|(scope, key, value)| [scope, "\0", key, "\n", value, "\0"])
            .collect::<String>()
            .into_bytes()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_repository_s_own_filters_are_emptied_and_lfs_and_the_user_s_are_kept() {
        let listing = scoped(&[
            ("global", "filter.lfs.clean", "git-lfs clean -- %f"),
            ("global", "filter.mine.clean", "my-filter"),
            ("local", "filter.lfs.process", "git-lfs filter-process"),
            ("local", "filter.Evil.clean", "sh -c 'touch x'"),
            ("worktree", "filter.a.b=c.process", "run"),
            ("local", "include.path", "extra"),
            ("local", "filter.x.required", "true"),
        ]);
        assert_eq!(
            overrides(&listing, true).unwrap(),
            pairs(&[
                ("filter.Evil.clean", ""),
                ("filter.a.b=c.process", ""),
                ("filter.Evil.required", "false"),
                ("filter.a.b=c.required", "false"),
            ])
        );
    }

    #[test]
    fn a_filter_with_an_empty_name_is_emptied_and_one_that_is_not_utf8_refused() {
        // `[filter ""]` is selected by `filter=` in `.gitattributes`.
        let listing = scoped(&[("local", "filter..clean", "touch x; cat")]);
        assert_eq!(
            overrides(&listing, true).unwrap(),
            pairs(&[("filter..clean", ""), ("filter..required", "false")])
        );
        let mut listing = b"local\0filter.".to_vec();
        listing.extend_from_slice(&[0xff, 0xfe]);
        listing.extend_from_slice(b".clean\ntouch x; cat\0");
        assert_eq!(overrides(&listing, true), Err(Refusal::NotUtf8));
        // Another key that is not UTF-8 changes nothing.
        let mut listing = b"local\0alias.".to_vec();
        listing.extend_from_slice(&[0xff]);
        listing.extend_from_slice(b"\nx\0");
        assert_eq!(overrides(&listing, true), Ok(Vec::new()));
    }

    #[test]
    fn a_git_lfs_extension_in_the_repository_empties_every_filter() {
        let listing = scoped(&[
            ("global", "filter.lfs.clean", "git-lfs clean -- %f"),
            ("system", "filter.mine.smudge", "my-filter"),
            ("local", "lfs.extension.evil.clean", "sh -c 'touch x'"),
        ]);
        let emptied: Vec<_> = overrides(&listing, true)
            .unwrap()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(
            emptied,
            [
                "filter.lfs.clean",
                "filter.mine.smudge",
                "filter.lfs.required",
                "filter.mine.required"
            ]
        );
        // A user's own extension changes nothing.
        let listing = scoped(&[
            ("global", "lfs.extension.mine.clean", "x"),
            ("global", "filter.lfs.clean", "git-lfs clean -- %f"),
        ]);
        assert!(overrides(&listing, true).unwrap().is_empty());
    }

    #[test]
    fn a_listing_without_scopes_counts_every_filter_as_the_repository_s() {
        let listing =
            b"filter.lfs.clean\ngit-lfs clean -- %f\0filter.mine.clean\nmine\0core.bare\nfalse\0";
        let emptied: Vec<_> = overrides(listing, false)
            .unwrap()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(emptied, ["filter.mine.clean", "filter.mine.required"]);
    }

    #[test]
    fn git_versions_are_read_from_every_build_s_wording() {
        assert_eq!(
            version("git version 2.39.5 (Apple Git-154)\n"),
            Some((2, 39))
        );
        assert_eq!(version("git version 2.45.1.windows.1"), Some((2, 45)));
        assert_eq!(version("git version 2.30.0-rc2"), Some((2, 30)));
        assert_eq!(version("hub version 2.14.2"), None);
    }

    #[test]
    fn overrides_reach_git_as_variables_from_2_31_and_as_arguments_before() {
        let emptied = pairs(&[("filter.x.clean", "")]);
        assert_eq!(
            carry(&emptied, Some((2, 31)), 2).unwrap(),
            (
                Vec::new(),
                pairs(&[
                    ("GIT_CONFIG_KEY_2", "filter.x.clean"),
                    ("GIT_CONFIG_VALUE_2", ""),
                    ("GIT_CONFIG_COUNT", "3"),
                ])
            )
        );
        // Git 2.30 ignores GIT_CONFIG_COUNT, so the filter would still run.
        for old in [Some((2, 30)), Some((1, 9)), None] {
            assert_eq!(
                carry(&emptied, old, 2).unwrap(),
                (
                    vec![OsString::from("-c"), OsString::from("filter.x.clean=")],
                    Vec::new()
                )
            );
        }
        // `-c` cannot name a filter whose name holds `=`.
        let equals = pairs(&[("filter.a=b.clean", "")]);
        assert_eq!(
            carry(&equals, Some((2, 30)), 0),
            Err(Refusal::NeedsNewerGit)
        );
        assert!(carry(&equals, Some((2, 31)), 0).is_ok());
        assert_eq!(carry(&[], Some((2, 30)), 0), Ok((Vec::new(), Vec::new())));
    }
}
