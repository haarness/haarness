//! Records the commit this binary was built from as `HAARNESS_BUILD_COMMIT`
//! (`<short sha>`, plus `-dirty` when tracked files had uncommitted changes)
//! for the startup log line. Without git, or outside this repository's own
//! checkout, the variable is left unset and the binary logs `unknown`. The
//! script never fails the build.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let Some(manifest) = env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else {
        return;
    };
    if let Some(commit) = commit(&manifest) {
        println!("cargo:rustc-env=HAARNESS_BUILD_COMMIT={commit}");
    }
}

fn commit(manifest: &Path) -> Option<String> {
    // A source tree without a `.git` of its own, unpacked inside another
    // repository, would read that repository's commit. Only this workspace's
    // own checkout counts.
    let top = git(manifest, &["rev-parse", "--show-toplevel"])?;
    let workspace = manifest.parent()?.parent()?;
    if fs::canonicalize(top).ok()? != fs::canonicalize(workspace).ok()? {
        return None;
    }
    watch_head(manifest);
    let sha = git(manifest, &["rev-parse", "--short=12", "HEAD"])?;
    // `--no-optional-locks` keeps the probe from rewriting the index under a
    // concurrent git command. The flag reflects the tree when this script last
    // ran: HEAD moves rerun it, file edits alone do not.
    let dirty = git(
        manifest,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
        ],
    )
    .is_some();
    Some(if dirty { format!("{sha}-dirty") } else { sha })
}

/// Rerun when HEAD moves: a checkout rewrites HEAD, a commit rewrites the
/// branch ref, and `git pack-refs` moves refs into `packed-refs`. Worktrees
/// keep HEAD per worktree and refs in the common directory; `--git-path`
/// resolves both. Only existing paths are watched, since Cargo reruns a
/// script on every build for a missing one.
fn watch_head(manifest: &Path) {
    let path = |name: &str| {
        git(manifest, &["rev-parse", "--git-path", name])
            .map(|path| manifest.join(path))
            .filter(|path| path.exists())
    };
    let mut watched = vec![path("HEAD"), path("packed-refs")];
    if let Some(reference) = git(manifest, &["symbolic-ref", "-q", "HEAD"]) {
        // A packed branch has no loose file until its next commit; its folder
        // changes when that file appears.
        let loose =
            git(manifest, &["rev-parse", "--git-path", &reference]).map(|path| manifest.join(path));
        watched.push(loose.and_then(|loose| {
            loose
                .ancestors()
                .find(|ancestor| ancestor.exists())
                .map(Path::to_path_buf)
        }));
    }
    for path in watched.into_iter().flatten() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// Trimmed standard output of a successful, non-empty git command.
fn git(manifest: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(manifest)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}
