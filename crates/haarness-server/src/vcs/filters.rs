//! The clean, smudge and process filters a review read empties.
//!
//! A repository's own configuration can name a program as a filter that its
//! `.gitattributes` selects, and Git runs it whenever a diff reads a changed
//! file from the working tree. A folder someone else prepared (a copied
//! checkout, an unpacked archive) brings that configuration with it, so the
//! review empties every filter the repository itself defines before Git
//! reads anything. Filters from the user's own global and system
//! configuration still run.

use std::collections::BTreeSet;

/// The commands `git lfs install` writes for its filter. They run the user's
/// installed git-lfs, so they are kept.
const LFS: [&str; 5] = [
    "git-lfs clean -- %f",
    "git-lfs smudge -- %f",
    "git-lfs smudge --skip -- %f",
    "git-lfs filter-process",
    "git-lfs filter-process --skip",
];

/// One entry of `git config -z --list`, with its scope when the listing was
/// made with `--show-scope`.
struct Entry<'a> {
    scope: Option<&'a str>,
    key: &'a str,
    value: &'a str,
}

/// The entries of a `-z --list` listing. With `scoped`, each entry follows
/// its scope (`--show-scope`); otherwise every entry counts as the
/// repository's own.
fn entries(listing: &str, scoped: bool) -> Vec<Entry<'_>> {
    let mut fields = listing.split('\0').filter(|field| !field.is_empty());
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
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        entries.push(Entry { scope, key, value });
    }
    entries
}

/// The filter a `filter.<name>.clean|smudge|process` key configures.
fn filter_name(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("filter.")?;
    ["clean", "smudge", "process"].iter().find_map(|kind| {
        rest.strip_suffix(kind)
            .and_then(|name| name.strip_suffix('.'))
            .filter(|name| !name.is_empty())
    })
}

/// The settings that empty every filter the repository's own configuration
/// defines, apart from the standard git-lfs commands, given a `-z --list`
/// listing (`scoped` when made with `--show-scope`). When that configuration
/// defines a git-lfs extension, git-lfs runs the program it names, so every
/// filter is emptied, git-lfs's and those of every scope included. Each
/// emptied filter is also made optional, so Git reads the file as it is.
pub(super) fn overrides(listing: &str, scoped: bool) -> Vec<(String, String)> {
    let entries = entries(listing, scoped);
    let own = |entry: &Entry<'_>| {
        entry
            .scope
            .is_none_or(|scope| matches!(scope, "local" | "worktree"))
    };
    let extension = entries.iter().filter(|entry| own(entry)).any(|entry| {
        entry.key.starts_with("lfs.extension.")
            && entry
                .key
                .rsplit_once('.')
                .is_some_and(|(_, kind)| matches!(kind, "clean" | "smudge"))
    });
    let mut keys = BTreeSet::new();
    let mut names = BTreeSet::new();
    for entry in &entries {
        let Some(name) = filter_name(entry.key) else {
            continue;
        };
        if extension || (own(entry) && !LFS.contains(&entry.value)) {
            keys.insert(entry.key.to_owned());
            names.insert(name.to_owned());
        }
    }
    keys.into_iter()
        .map(|key| (key, String::new()))
        .chain(
            names
                .into_iter()
                .map(|name| (format!("filter.{name}.required"), "false".to_owned())),
        )
        .collect()
}

/// `GIT_CONFIG_*` variables that set `pairs` after the `base` pairs already
/// in the environment. They take any key, unlike `-c`, which cannot name a
/// filter whose name holds `=`.
pub(super) fn environment(pairs: &[(String, String)], base: usize) -> Vec<(String, String)> {
    if pairs.is_empty() {
        return Vec::new();
    }
    let mut variables = Vec::with_capacity(pairs.len() * 2 + 1);
    for (index, (key, value)) in pairs.iter().enumerate() {
        variables.push((format!("GIT_CONFIG_KEY_{}", base + index), key.clone()));
        variables.push((format!("GIT_CONFIG_VALUE_{}", base + index), value.clone()));
    }
    variables.push((
        "GIT_CONFIG_COUNT".to_owned(),
        (base + pairs.len()).to_string(),
    ));
    variables
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scoped(entries: &[(&str, &str, &str)]) -> String {
        entries
            .iter()
            .flat_map(|(scope, key, value)| [scope, "\0", key, "\n", value, "\0"])
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
            overrides(&listing, true),
            [
                ("filter.Evil.clean", ""),
                ("filter.a.b=c.process", ""),
                ("filter.Evil.required", "false"),
                ("filter.a.b=c.required", "false"),
            ]
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
        );
    }

    #[test]
    fn a_git_lfs_extension_in_the_repository_empties_every_filter() {
        let listing = scoped(&[
            ("global", "filter.lfs.clean", "git-lfs clean -- %f"),
            ("system", "filter.mine.smudge", "my-filter"),
            ("local", "lfs.extension.evil.clean", "sh -c 'touch x'"),
        ]);
        let emptied: Vec<_> = overrides(&listing, true)
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
        assert!(overrides(&listing, true).is_empty());
    }

    #[test]
    fn a_listing_without_scopes_counts_every_filter_as_the_repository_s() {
        let listing =
            "filter.lfs.clean\ngit-lfs clean -- %f\0filter.mine.clean\nmine\0core.bare\nfalse\0";
        let emptied: Vec<_> = overrides(listing, false)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(emptied, ["filter.mine.clean", "filter.mine.required"]);
    }

    #[test]
    fn the_settings_follow_the_pairs_already_in_the_environment() {
        let pairs = vec![("filter.x.clean".to_owned(), String::new())];
        assert_eq!(
            environment(&pairs, 2),
            [
                ("GIT_CONFIG_KEY_2", "filter.x.clean"),
                ("GIT_CONFIG_VALUE_2", ""),
                ("GIT_CONFIG_COUNT", "3"),
            ]
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
        );
        assert!(environment(&[], 0).is_empty());
    }
}
