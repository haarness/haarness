use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("haarness-profile-test-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        Self(dunce::canonicalize(&root).unwrap())
    }
    fn roots(&self, name: &str) -> Roots {
        let root = self.0.join(name);
        let roots = Roots {
            config: root.join("config"),
            data: root.join("data"),
            project: root.join("project"),
        };
        for path in [&roots.config, &roots.data, &roots.project] {
            fs::create_dir_all(path).unwrap();
        }
        roots
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn put(root: &Path, path: &str, data: &str) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, data).unwrap();
}

#[test]
fn scoped_roundtrip_preserves_assets_and_excludes_runtime_credentials_and_unrelated_files() {
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(
        &source.config,
        "config.toml",
        "[defaults]\nagent = 'review'\n",
    );
    put(&source.config, "agents/reviewer.md", "review instructions");
    put(&source.config, "hooks/check.sh", "echo checked");
    put(&source.config, "auth.json", "secret");
    put(&source.config, "scheduler/jobs.json", "must not reschedule");
    put(&source.data, "skills/a/SKILL.md", "skill");
    put(&source.data, "haarness.db", "history");
    put(
        &source.project,
        ".haarness/commands/test.md",
        "test instructions",
    );
    put(&source.project, ".haarness/skills/a/.env", "secret");
    put(
        &source.project,
        ".haarness/skills/a/node_modules/big.js",
        "dependency",
    );
    put(&source.project, "AGENTS.md", "project instructions");
    put(&source.project, "main.rs", "unrelated source");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            source.config.join("hooks/check.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    for (name, scope, count) in [
        ("both", Scope::Both, 6),
        ("global", Scope::Global, 4),
        ("local", Scope::Local, 2),
    ] {
        let bytes = Archive::capture(&source, name, scope).unwrap().bytes;
        let archive = Archive::decode(&bytes).unwrap();
        assert_eq!(archive.files.len(), count);
        let target = fixture.roots(name);
        archive.restore(&target, false, true).unwrap();
        assert!(!target.config.join("config.toml").exists());
        archive.restore(&target, false, false).unwrap();
        for (path, data) in &archive.files {
            assert_eq!(
                fs::read(super::target(&target, path).unwrap()).unwrap(),
                *data
            );
        }
        assert!(!target.config.join("auth.json").exists());
        assert!(!target.data.join("haarness.db").exists());
        assert!(!target.project.join("main.rs").exists());
        if scope != Scope::Local {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(target.config.join("hooks/check.sh"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
                assert_eq!(
                    fs::metadata(target.config.join("config.toml"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
    }
}

#[test]
fn a_keychain_reference_travels_as_written_and_never_its_secret() {
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    let settings = r#"{"mcp": {"github": {"type": "remote", "url": "https://mcp.example.test",
        "headers": {"Authorization": "Bearer ${keychain:github-token}"}}}}"#;
    put(&source.config, "haarness.json", settings);
    let bytes = Archive::capture(&source, "keychain", Scope::Global)
        .unwrap()
        .bytes;
    let archive = Archive::decode(&bytes).unwrap();
    assert_eq!(
        archive.files["global/config/haarness.json"],
        settings.as_bytes(),
        "the reference is kept as written"
    );
    let target = fixture.roots("target");
    archive.restore(&target, false, false).unwrap();
    assert_eq!(
        fs::read_to_string(target.config.join("haarness.json")).unwrap(),
        settings
    );
}

#[test]
fn export_leaves_out_what_finder_and_macos_write_beside_configuration() {
    let fixture = Fixture::new();
    let roots = fixture.roots("source");
    put(&roots.config, "agents/reviewer.md", "review instructions");
    put(&roots.config, "agents/.DS_Store", "finder view settings");
    put(&roots.config, "agents/._reviewer.md", "appledouble");
    put(&roots.data, "skills/a/SKILL.md", "skill");
    put(&roots.data, "skills/a/__MACOSX/._SKILL.md", "appledouble");
    put(&roots.data, "skills/.DS_Store", "finder view settings");
    put(&roots.project, ".haarness/commands/test.md", "test");
    put(
        &roots.project,
        ".haarness/commands/._test.md",
        "appledouble",
    );
    let archive =
        Archive::decode(&Archive::capture(&roots, "mac", Scope::Both).unwrap().bytes).unwrap();
    let names: Vec<_> = archive.files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        vec![
            "global/config/agents/reviewer.md",
            "global/config/skills/a/SKILL.md",
            "project/.haarness/commands/test.md",
        ]
    );
}

#[test]
fn project_profile_never_captures_claude_code_files() {
    let fixture = Fixture::new();
    let roots = fixture.roots("source");
    put(
        &roots.project,
        ".haarness/commands/visible.md",
        "haarness command",
    );
    put(&roots.project, ".claude/settings.json", "claude config");
    put(
        &roots.project,
        ".claude/skills/ignored/SKILL.md",
        "claude skill",
    );
    put(&roots.project, "CLAUDE.md", "claude instructions");
    put(&roots.project, ".mcp.json", "claude mcp");
    let archive = Archive::decode(
        &Archive::capture(&roots, "project", Scope::Local)
            .unwrap()
            .bytes,
    )
    .unwrap();
    let names: Vec<_> = archive.files.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["project/.haarness/commands/visible.md"]);
}

#[test]
fn conflict_preflight_changes_nothing_and_force_replaces_only_listed_files() {
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(&source.config, "AGENTS.md", "new instructions");
    put(&source.project, "haarness.json", "{}");
    let archive = Archive::decode(
        &Archive::capture(&source, "conflict", Scope::Both)
            .unwrap()
            .bytes,
    )
    .unwrap();
    let destination = fixture.roots("destination");
    put(&destination.project, "haarness.json", "original");
    put(&destination.config, "agents/keep.md", "keep");
    assert!(
        archive
            .restore(&destination, false, false)
            .unwrap_err()
            .to_string()
            .contains("overwrite")
    );
    assert!(!destination.config.join("AGENTS.md").exists());
    assert_eq!(
        fs::read_to_string(destination.project.join("haarness.json")).unwrap(),
        "original"
    );
    archive.restore(&destination, true, false).unwrap();
    assert_eq!(
        fs::read_to_string(destination.config.join("agents/keep.md")).unwrap(),
        "keep"
    );
    assert_eq!(
        fs::read_to_string(destination.project.join("haarness.json")).unwrap(),
        "{}"
    );
}

#[test]
fn usage_statistics_consent_stays_with_each_machine() {
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(
        &source.config,
        "haarness.json",
        "{\n  // shared model\n  \"model\": \"echo/echo-1\",\n  \"telemetry\": { \"usage\": true }\n}\n",
    );
    let bytes = Archive::capture(&source, "consent", Scope::Global)
        .unwrap()
        .bytes;
    let mut archive = Archive::decode(&bytes).unwrap();
    let saved = String::from_utf8(archive.files["global/config/haarness.json"].clone()).unwrap();
    assert!(!saved.contains("telemetry"), "{saved}");
    assert!(saved.contains("// shared model") && saved.contains("echo/echo-1"));

    // An archive saved before consent was left out still keeps the target's
    // own choice, and never adds one the target did not make.
    archive.files.insert(
        "global/config/haarness.json".into(),
        br#"{"model":"echo/echo-2","telemetry":{"usage":true}}"#.to_vec(),
    );
    let destination = fixture.roots("destination");
    put(
        &destination.config,
        "haarness.json",
        r#"{"telemetry":{"usage":false}}"#,
    );
    archive.restore(&destination, true, false).unwrap();
    let restored: serde_json::Value =
        serde_json::from_slice(&fs::read(destination.config.join("haarness.json")).unwrap())
            .unwrap();
    assert_eq!(restored["model"], "echo/echo-2");
    assert_eq!(restored["telemetry"]["usage"], false);

    let fresh = fixture.roots("fresh");
    archive.restore(&fresh, false, false).unwrap();
    let restored: serde_json::Value =
        serde_json::from_slice(&fs::read(fresh.config.join("haarness.json")).unwrap()).unwrap();
    assert_eq!(restored["model"], "echo/echo-2");
    assert!(restored.get("telemetry").is_none());
}

#[test]
fn dry_run_lists_every_target_and_a_refusal_names_every_conflict() {
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(&source.config, "AGENTS.md", "new instructions");
    put(&source.config, "agents/a.md", "a");
    put(&source.project, "haarness.json", "{}");
    let archive = Archive::decode(
        &Archive::capture(&source, "conflicts", Scope::Both)
            .unwrap()
            .bytes,
    )
    .unwrap();
    let destination = fixture.roots("destination");
    put(&destination.config, "AGENTS.md", "mine");
    put(&destination.project, "haarness.json", "mine");
    let targets = archive.restore(&destination, false, true).unwrap().targets;
    assert_eq!(targets.len(), 3);
    let error = archive
        .restore(&destination, false, false)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("AGENTS.md") && error.contains("haarness.json"),
        "{error}"
    );
    assert!(!destination.config.join("agents/a.md").exists());
}

fn crafted(
    path: &str,
    version: u32,
    scope: Scope,
    checksum: &str,
    symlink: bool,
    extra: bool,
) -> Vec<u8> {
    let manifest = Manifest {
        format: "haarness.profile".into(),
        version,
        name: "fixture".into(),
        created_at: Utc::now(),
        scope,
        entries: vec![Entry {
            path: path.into(),
            size: 1,
            sha256: checksum.into(),
            executable: false,
        }],
    };
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    zip.start_file("manifest.json", options).unwrap();
    zip.write_all(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    if symlink {
        zip.add_symlink(path, "x", options).unwrap();
    } else {
        zip.start_file(path, options).unwrap();
        zip.write_all(b"x").unwrap();
    }
    if extra {
        zip.start_file("extra", options).unwrap();
        zip.write_all(b"x").unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn untrusted_archives_reject_traversal_credentials_collisions_bad_hashes_links_and_unknown_versions()
 {
    for path in [
        "../outside",
        "/project/AGENTS.md",
        "project/../AGENTS.md",
        "project/C:/AGENTS.md",
        "project/AGENTS.md ",
        "global/config/auth.json",
        "project/.haarness/skills/a/.env",
        "project/main.rs",
        "project/.haarness/agents/CON.md",
        "project/.haarness/agents/a\\b",
    ] {
        assert!(
            Archive::decode(&crafted(path, 1, Scope::Both, &hash(b"x"), false, false)).is_err(),
            "accepted {path}"
        );
    }
    for bytes in [
        crafted(
            "project/AGENTS.md",
            9,
            Scope::Both,
            &hash(b"x"),
            false,
            false,
        ),
        crafted(
            "project/AGENTS.md",
            1,
            Scope::Global,
            &hash(b"x"),
            false,
            false,
        ),
        crafted("project/AGENTS.md", 1, Scope::Both, "wrong", false, false),
        crafted(
            "project/AGENTS.md",
            1,
            Scope::Both,
            &hash(b"x"),
            true,
            false,
        ),
        crafted(
            "project/AGENTS.md",
            1,
            Scope::Both,
            &hash(b"x"),
            false,
            true,
        ),
    ] {
        assert!(Archive::decode(&bytes).is_err());
    }
    assert!(Archive::decode(b"not a zip").is_err());
}

#[cfg(unix)]
#[test]
fn source_and_destination_symlinks_cannot_escape_configuration_roots() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(&source.config, "agents/review.md", "new");
    let archive = Archive::decode(
        &Archive::capture(&source, "links", Scope::Global)
            .unwrap()
            .bytes,
    )
    .unwrap();
    let destination = fixture.roots("destination");
    let outside = fixture.0.join("outside");
    put(&outside, "review.md", "original");
    symlink(&outside, destination.config.join("agents")).unwrap();
    assert!(archive.restore(&destination, true, false).is_err());
    assert_eq!(
        fs::read_to_string(outside.join("review.md")).unwrap(),
        "original"
    );
    // A saved folder linked to one outside is saved without it, and named.
    fs::remove_dir_all(source.config.join("agents")).unwrap();
    symlink(&outside, source.config.join("agents")).unwrap();
    let saved = Archive::capture(&source, "links", Scope::Global).unwrap();
    assert_eq!(saved.left_out, [source.config.join("agents")]);
    assert!(Archive::decode(&saved.bytes).unwrap().files.is_empty());
}

#[cfg(unix)]
#[test]
fn links_inside_the_saved_folders_are_saved_as_the_files_they_lead_to() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    // A skill shared by two agents' folders, instructions kept under another
    // name, and a whole folder that is a link to another.
    put(&source.project, ".haarness/skills/fmt/SKILL.md", "format");
    put(&source.project, "CLAUDE.md", "project instructions");
    fs::create_dir_all(source.project.join(".agents/skills")).unwrap();
    symlink(
        "../../.haarness/skills/fmt",
        source.project.join(".agents/skills/fmt"),
    )
    .unwrap();
    symlink("CLAUDE.md", source.project.join("AGENTS.md")).unwrap();
    symlink(".haarness", source.project.join(".opencode")).unwrap();
    // In the configuration folder, a link to a part a profile saves.
    put(&source.config, "agents/review.md", "review");
    fs::create_dir_all(source.config.join("commands")).unwrap();
    symlink(
        "../agents/review.md",
        source.config.join("commands/review.md"),
    )
    .unwrap();
    // An installed plugin whose recorded folder is a link.
    let plugin = fixture.0.join("plugin-source");
    put(
        &plugin,
        ".claude-plugin/plugin.json",
        "{\"name\":\"linked\"}",
    );
    symlink(&plugin, fixture.0.join("plugin-link")).unwrap();
    put(
        &source.data,
        "plugins/installed.json",
        &serde_json::json!({"linked":{"path":fixture.0.join("plugin-link"),"enabled":true}})
            .to_string(),
    );
    let saved = Archive::capture(&source, "links", Scope::Both).unwrap();
    assert!(saved.left_out.is_empty(), "{:?}", saved.left_out);
    let archive = Archive::decode(&saved.bytes).unwrap();
    for (path, data) in [
        ("project/.haarness/skills/fmt/SKILL.md", "format"),
        ("project/.agents/skills/fmt/SKILL.md", "format"),
        ("project/.opencode/skills/fmt/SKILL.md", "format"),
        ("project/AGENTS.md", "project instructions"),
        ("global/config/agents/review.md", "review"),
        ("global/config/commands/review.md", "review"),
    ] {
        assert_eq!(
            archive.files.get(path).map(Vec::as_slice),
            Some(data.as_bytes()),
            "{path}"
        );
    }
    assert!(
        archive
            .files
            .keys()
            .any(|path| path.ends_with("/.claude-plugin/plugin.json"))
    );
    // A restore writes regular files, never links.
    let destination = fixture.roots("destination");
    archive.restore(&destination, false, false).unwrap();
    for path in [
        destination.project.join(".agents/skills/fmt/SKILL.md"),
        destination.project.join(".opencode/skills/fmt/SKILL.md"),
        destination.project.join("AGENTS.md"),
        destination.config.join("commands/review.md"),
    ] {
        assert!(
            fs::symlink_metadata(&path).unwrap().is_file(),
            "{}",
            path.display()
        );
    }
    assert!(
        !fs::symlink_metadata(destination.project.join(".opencode"))
            .unwrap()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn links_that_lead_outside_the_saved_folders_are_left_out_and_named() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    let dotfiles = fixture.0.join("dotfiles");
    put(&dotfiles, "skills/fmt/SKILL.md", "dotfiles skill");
    put(&dotfiles, "skills/lint/SKILL.md", "dotfiles skill");
    put(&dotfiles, "AGENTS.md", "dotfiles instructions");
    put(
        &dotfiles,
        "plugins/local/tool/plugin.json",
        "dotfiles plugin",
    );
    put(&source.config, "skills/own/SKILL.md", "own skill");
    // Skills and instructions linked in from a dotfiles repository, the data
    // folder's skills and plugins folders linked there whole, and a project
    // skill linked there too.
    symlink(
        dotfiles.join("skills/fmt"),
        source.config.join("skills/fmt"),
    )
    .unwrap();
    symlink(dotfiles.join("AGENTS.md"), source.config.join("AGENTS.md")).unwrap();
    symlink(dotfiles.join("skills"), source.data.join("skills")).unwrap();
    fs::create_dir_all(source.config.join("plugins")).unwrap();
    symlink(
        dotfiles.join("plugins/local"),
        source.config.join("plugins/local"),
    )
    .unwrap();
    fs::create_dir_all(source.project.join(".haarness/skills")).unwrap();
    symlink(
        dotfiles.join("skills/lint"),
        source.project.join(".haarness/skills/lint"),
    )
    .unwrap();
    // A link to a part of the configuration folder a profile never saves.
    put(
        &source.config,
        "tui-servers.json",
        "{\"token\":\"server secret\"}",
    );
    fs::create_dir_all(source.config.join("skills/servers")).unwrap();
    symlink(
        "../../tui-servers.json",
        source.config.join("skills/servers/SKILL.md"),
    )
    .unwrap();
    let saved = Archive::capture(&source, "outside", Scope::Both).unwrap();
    let mut expected = vec![
        source.config.join("AGENTS.md"),
        source.config.join("plugins/local"),
        source.config.join("skills/fmt"),
        source.config.join("skills/servers/SKILL.md"),
        source.data.join("skills"),
        source.project.join(".haarness/skills/lint"),
    ];
    expected.sort();
    assert_eq!(saved.left_out, expected);
    let archive = Archive::decode(&saved.bytes).unwrap();
    let names: Vec<_> = archive.files.keys().map(String::as_str).collect();
    assert_eq!(names, ["global/config/skills/own/SKILL.md"]);
}

#[cfg(unix)]
#[test]
fn a_link_never_brings_in_a_file_a_profile_leaves_out() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(&source.project, ".env", "API_KEY=project secret");
    put(&source.project, "certs/server.key", "key secret");
    put(&source.project, ".haarness/skills/a/SKILL.md", "skill");
    symlink(
        "../../../.env",
        source.project.join(".haarness/skills/a/notes.md"),
    )
    .unwrap();
    symlink(
        "../../../certs/server.key",
        source.project.join(".haarness/skills/a/cert.md"),
    )
    .unwrap();
    put(&source.config, "auth.json", "{\"key\":\"provider secret\"}");
    fs::create_dir_all(source.config.join("agents")).unwrap();
    symlink("../auth.json", source.config.join("agents/auth.md")).unwrap();
    put(&source.data, "haarness.db", "chat history");
    put(&source.data, "skills/b/SKILL.md", "skill");
    symlink("../../haarness.db", source.data.join("skills/b/history.md")).unwrap();
    let saved = Archive::capture(&source, "secrets", Scope::Both).unwrap();
    let mut expected = vec![
        source.config.join("agents/auth.md"),
        source.data.join("skills/b/history.md"),
        source.project.join(".haarness/skills/a/cert.md"),
        source.project.join(".haarness/skills/a/notes.md"),
    ];
    expected.sort();
    assert_eq!(saved.left_out, expected);
    let archive = Archive::decode(&saved.bytes).unwrap();
    assert_eq!(archive.files.len(), 2, "{:?}", archive.files.keys());
    assert!(archive.files.values().all(|data| data == b"skill"));
}

#[cfg(unix)]
#[test]
fn links_that_loop_or_lead_nowhere_never_stop_a_save() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    let (a, b) = (
        source.project.join(".haarness/skills/a"),
        source.project.join(".agents/skills/b"),
    );
    put(&a, "SKILL.md", "skill a");
    put(&b, "SKILL.md", "skill b");
    // Back to a folder that holds the link, to the link itself, in a circle
    // through two folders, to the whole project, and to a file that is gone.
    symlink("..", a.join("up")).unwrap();
    symlink("self", a.join("self")).unwrap();
    symlink("../../../.agents/skills/b", a.join("b")).unwrap();
    symlink("../../../.haarness/skills/a", b.join("a")).unwrap();
    symlink("../../..", b.join("project")).unwrap();
    symlink("gone.md", a.join("missing.md")).unwrap();
    let saved = Archive::capture(&source, "loops", Scope::Local).unwrap();
    assert!(saved.left_out.is_empty(), "{:?}", saved.left_out);
    let archive = Archive::decode(&saved.bytes).unwrap();
    let names: Vec<_> = archive.files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "project/.agents/skills/b/SKILL.md",
            "project/.agents/skills/b/a/SKILL.md",
            "project/.haarness/skills/a/SKILL.md",
            "project/.haarness/skills/a/b/SKILL.md",
        ]
    );
}

#[cfg(unix)]
#[test]
fn links_that_fan_out_into_one_another_stop_at_a_limit() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    // Each folder links twice to the next, so the last is reached 2^24 ways.
    let skills = source.project.join(".haarness/skills");
    for level in 0..24 {
        let folder = skills.join(format!("l{level}"));
        fs::create_dir_all(&folder).unwrap();
        for name in ["a", "b"] {
            symlink(format!("../l{}", level + 1), folder.join(name)).unwrap();
        }
    }
    fs::create_dir_all(skills.join("l24")).unwrap();
    let started = std::time::Instant::now();
    let error = Archive::capture(&source, "fan out", Scope::Local)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("symbolic links"), "{error}");
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
}

#[cfg(unix)]
#[test]
fn a_project_link_brings_in_only_what_a_profile_saves_or_markdown() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    let project = &source.project;
    // A cloned repository ships links to files only this computer has.
    put(project, ".dev.vars", "TOKEN=dev secret");
    put(project, ".envrc", "export TOKEN=envrc secret");
    put(
        project,
        ".npmrc",
        "//registry.example.test/:_authToken=npm secret",
    );
    put(project, "secrets/prod.tfvars", "password = \"prod secret\"");
    put(project, ".haarness/skills/deploy/SKILL.md", "deploy");
    symlink(".dev.vars", project.join("AGENTS.md")).unwrap();
    let deploy = project.join(".haarness/skills/deploy");
    symlink("../../../.envrc", deploy.join("envrc.md")).unwrap();
    symlink("../../../.npmrc", deploy.join("npmrc")).unwrap();
    symlink("../../../secrets", deploy.join("secrets")).unwrap();
    // Instructions kept as Markdown elsewhere in the project still come in.
    put(project, "docs/agents.md", "shared instructions");
    symlink("../docs/agents.md", project.join(".haarness/AGENTS.md")).unwrap();
    // But not as a file where a profile saves only a folder.
    fs::create_dir_all(project.join(".agents")).unwrap();
    symlink("../docs/agents.md", project.join(".agents/commands")).unwrap();
    let saved = Archive::capture(&source, "cloned", Scope::Local).unwrap();
    let mut expected = vec![
        project.join(".agents/commands"),
        project.join("AGENTS.md"),
        deploy.join("envrc.md"),
        deploy.join("npmrc"),
        deploy.join("secrets"),
    ];
    expected.sort();
    assert_eq!(saved.left_out, expected);
    let archive = Archive::decode(&saved.bytes).unwrap();
    let names: Vec<_> = archive.files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "project/.haarness/AGENTS.md",
            "project/.haarness/skills/deploy/SKILL.md"
        ]
    );
    assert_eq!(
        archive.files["project/.haarness/AGENTS.md"],
        b"shared instructions"
    );
}

#[cfg(unix)]
#[test]
fn saving_from_the_home_folder_never_follows_a_link_into_its_private_folders() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let home = fixture.0.join("home");
    let roots = Roots {
        config: home.join(".config/haarness"),
        data: home.join(".local/share/haarness"),
        project: home.clone(),
    };
    for path in [&roots.config, &roots.data] {
        fs::create_dir_all(path).unwrap();
    }
    put(&home, ".ssh/id_ed25519", "ssh secret");
    put(&home, ".aws/credentials", "aws secret");
    put(&roots.config, "tui-servers.json", "server secret");
    put(&roots.config, "skills/own/SKILL.md", "own");
    let cloud = home.join(".agents/skills/cloud");
    put(&cloud, "SKILL.md", "cloud");
    symlink("../../../.ssh/id_ed25519", cloud.join("key.md")).unwrap();
    symlink("../../../.aws", cloud.join("aws")).unwrap();
    symlink(
        "../../../.config/haarness/tui-servers.json",
        cloud.join("servers.md"),
    )
    .unwrap();
    let saved = Archive::capture(&roots, "home", Scope::Both).unwrap();
    let mut expected = vec![
        cloud.join("aws"),
        cloud.join("key.md"),
        cloud.join("servers.md"),
    ];
    expected.sort();
    assert_eq!(saved.left_out, expected);
    let archive = Archive::decode(&saved.bytes).unwrap();
    let names: Vec<_> = archive.files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "global/config/skills/own/SKILL.md",
            "project/.agents/skills/cloud/SKILL.md"
        ]
    );
}

/// A link that led outside when the file was opened, then was put back to
/// look harmless, is judged by the file that was opened.
#[cfg(unix)]
#[test]
fn a_file_is_judged_by_what_was_opened_not_by_its_path() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    let secret = fixture.0.join("outside/id_race");
    put(&fixture.0, "outside/id_race", "race secret");
    put(&source.project, ".haarness/skills/x/SKILL.md", "skill");
    let link = source.project.join(".haarness/skills/x/r.md");
    symlink(&secret, &link).unwrap();
    let file = open(&link).unwrap();
    fs::remove_file(&link).unwrap();
    symlink("SKILL.md", &link).unwrap();
    let project = Place::new(
        &source.project,
        "project",
        Reach::Project { markdown: true },
    );
    let opened = held(&file, &link);
    assert!(
        opened
            .as_deref()
            .is_none_or(|opened| project.inside(opened).is_none()),
        "{opened:?}"
    );
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    assert_eq!(opened, Some(secret));
}

/// While something swaps a skill's file between itself, a link inside and a
/// link to a secret outside, no save ever carries the secret.
#[cfg(unix)]
#[test]
fn a_link_swapped_while_saving_never_brings_in_a_file_outside() {
    use std::{
        os::unix::fs::symlink,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(&fixture.0, "outside/id_race", "race secret");
    let skill = source.project.join(".haarness/skills/x");
    put(&skill, "SKILL.md", "skill");
    put(&skill, "r.md", "harmless");
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let (stop, skill, secret, next) = (
            stop.clone(),
            skill.clone(),
            fixture.0.join("outside/id_race"),
            fixture.0.join("next"),
        );
        std::thread::spawn(move || {
            let mut turn = 0_usize;
            while !stop.load(Ordering::Relaxed) {
                let _ = fs::remove_file(&next);
                match turn % 3 {
                    0 => symlink(&secret, &next).unwrap(),
                    1 => symlink("SKILL.md", &next).unwrap(),
                    _ => fs::write(&next, "harmless").unwrap(),
                }
                fs::rename(&next, skill.join("r.md")).unwrap();
                turn += 1;
            }
        })
    };
    // A save may fail when a file changes under it; one that succeeds
    // never carries the secret.
    let mut saved = 0;
    for _ in 0..150 {
        let Ok(capture) = Archive::capture(&source, "race", Scope::Local) else {
            continue;
        };
        saved += 1;
        let archive = Archive::decode(&capture.bytes).unwrap();
        assert!(
            archive.files.values().all(|data| data != b"race secret"),
            "a save carried the secret"
        );
    }
    stop.store(true, Ordering::Relaxed);
    swapper.join().unwrap();
    assert!(saved > 0, "no save succeeded");
}

#[test]
fn only_a_link_that_leads_nowhere_is_passed_over_quietly() {
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(&source.project, ".haarness/skills/x/SKILL.md", "skill");
    #[cfg(unix)]
    {
        // A link inside a saved folder whose file is gone, and a saved
        // folder that is such a link.
        use std::os::unix::fs::symlink;
        symlink(
            "gone.md",
            source.project.join(".haarness/skills/x/notes.md"),
        )
        .unwrap();
        symlink("gone", source.project.join(".agents")).unwrap();
        let saved = Archive::capture(&source, "gone", Scope::Local).unwrap();
        assert!(saved.left_out.is_empty(), "{:?}", saved.left_out);
        let archive = Archive::decode(&saved.bytes).unwrap();
        let names: Vec<_> = archive.files.keys().map(String::as_str).collect();
        assert_eq!(names, ["project/.haarness/skills/x/SKILL.md"]);
    }
    // Anything else that cannot be resolved is an error, not an empty save.
    let project = Place::new(
        &source.project,
        "project",
        Reach::Project { markdown: true },
    );
    assert!(
        project
            .judge(&source.project.join(".opencode"), Scope::Local, &[])
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn a_link_brings_in_markdown_only_from_the_projects_own_files() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    // A project that is the home folder brings in no Markdown by a link.
    let home = fixture.0.join("home");
    let roots = Roots {
        config: home.join(".config/haarness"),
        data: home.join(".local/share/haarness"),
        project: home.clone(),
    };
    for path in [&roots.config, &roots.data] {
        fs::create_dir_all(path).unwrap();
    }
    put(&home, ".ssh/README.md", "ssh notes");
    put(&home, "Documents/vault/passwords.md", "password notes");
    let notes = home.join(".agents/skills/notes");
    put(&notes, "SKILL.md", "notes");
    symlink("../../../.ssh/README.md", notes.join("ssh.md")).unwrap();
    symlink(
        "../../../Documents/vault/passwords.md",
        notes.join("vault.md"),
    )
    .unwrap();
    let saved = Archive::capture_from(&roots, "home", Scope::Local, Some(&home)).unwrap();
    assert_eq!(
        saved.left_out,
        [notes.join("ssh.md"), notes.join("vault.md")]
    );
    let archive = Archive::decode(&saved.bytes).unwrap();
    let names: Vec<_> = archive.files.keys().map(String::as_str).collect();
    assert_eq!(names, ["project/.agents/skills/notes/SKILL.md"]);
    // Nor from a hidden folder or a `Library` in any other project.
    let source = fixture.roots("source");
    put(&source.project, ".github/notes.md", "hidden notes");
    put(&source.project, "Library/notes.md", "library notes");
    put(&source.project, "docs/agents.md", "shared instructions");
    let skill = source.project.join(".haarness/skills/notes");
    put(&skill, "SKILL.md", "notes");
    symlink("../../../.github/notes.md", skill.join("hidden.md")).unwrap();
    symlink("../../../Library/notes.md", skill.join("library.md")).unwrap();
    symlink("../../../docs/agents.md", skill.join("agents.md")).unwrap();
    let saved = Archive::capture_from(&source, "project", Scope::Local, Some(&home)).unwrap();
    assert_eq!(
        saved.left_out,
        [skill.join("hidden.md"), skill.join("library.md")]
    );
    let archive = Archive::decode(&saved.bytes).unwrap();
    assert_eq!(
        archive.files["project/.haarness/skills/notes/agents.md"],
        b"shared instructions"
    );
}

#[cfg(unix)]
#[test]
fn a_profile_restores_into_the_project_it_was_saved_from() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    let project = &source.project;
    put(project, ".haarness/skills/fmt/SKILL.md", "format");
    put(project, "CLAUDE.md", "project instructions");
    fs::create_dir_all(project.join(".agents/skills")).unwrap();
    symlink(
        "../../.haarness/skills/fmt",
        project.join(".agents/skills/fmt"),
    )
    .unwrap();
    symlink("CLAUDE.md", project.join("AGENTS.md")).unwrap();
    let archive = Archive::decode(
        &Archive::capture(&source, "same", Scope::Local)
            .unwrap()
            .bytes,
    )
    .unwrap();
    // The files the links lead to already hold the profile's bytes: they are
    // left alone, and only the folder's own copy is written.
    let in_place = [
        project.join(".agents/skills/fmt/SKILL.md"),
        project.join("AGENTS.md"),
    ];
    let planned = archive.restore(&source, true, true).unwrap();
    assert_eq!(
        planned.targets,
        [project.join(".haarness/skills/fmt/SKILL.md")]
    );
    assert_eq!(planned.in_place, in_place);
    let restored = archive.restore(&source, true, false).unwrap();
    assert_eq!(restored.in_place, in_place);
    assert!(
        fs::symlink_metadata(project.join(".agents/skills/fmt"))
            .unwrap()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(project.join("AGENTS.md"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(project.join("CLAUDE.md")).unwrap(),
        "project instructions"
    );
}

#[cfg(unix)]
#[test]
fn a_restore_still_refuses_a_link_to_other_bytes_or_outside_its_folder() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(&source.project, ".agents/skills/fmt/SKILL.md", "format");
    let archive = Archive::decode(
        &Archive::capture(&source, "links", Scope::Local)
            .unwrap()
            .bytes,
    )
    .unwrap();
    // A clone whose linked skill holds other bytes.
    let clone = fixture.roots("clone");
    put(
        &clone.project,
        ".haarness/skills/fmt/SKILL.md",
        "older format",
    );
    fs::create_dir_all(clone.project.join(".agents/skills")).unwrap();
    symlink(
        "../../.haarness/skills/fmt",
        clone.project.join(".agents/skills/fmt"),
    )
    .unwrap();
    // A project whose skill is linked to the same bytes outside it.
    let linked = fixture.roots("linked");
    let outside = fixture.0.join("outside");
    put(&outside, "fmt/SKILL.md", "format");
    fs::create_dir_all(linked.project.join(".agents/skills")).unwrap();
    symlink(
        outside.join("fmt"),
        linked.project.join(".agents/skills/fmt"),
    )
    .unwrap();
    for target in [&clone, &linked] {
        for dry_run in [true, false] {
            let error = archive
                .restore(target, true, dry_run)
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains("symlink path refused"), "{error}");
        }
    }
    assert_eq!(
        fs::read_to_string(clone.project.join(".haarness/skills/fmt/SKILL.md")).unwrap(),
        "older format"
    );
}

/// A file reached through a link is not in place when another file of the
/// profile, with other bytes, would be written over it in the same restore.
#[cfg(unix)]
#[test]
fn a_restore_refuses_a_file_in_place_that_it_would_then_overwrite() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(
        &source.project,
        ".agents/skills/fmt/SKILL.md",
        "team version",
    );
    put(
        &source.project,
        ".haarness/skills/fmt/SKILL.md",
        "personal version",
    );
    let archive = Archive::decode(
        &Archive::capture(&source, "copies", Scope::Local)
            .unwrap()
            .bytes,
    )
    .unwrap();
    let target = fixture.roots("target");
    put(
        &target.project,
        ".haarness/skills/fmt/SKILL.md",
        "team version",
    );
    fs::create_dir_all(target.project.join(".agents/skills")).unwrap();
    symlink(
        "../../.haarness/skills/fmt",
        target.project.join(".agents/skills/fmt"),
    )
    .unwrap();
    for dry_run in [true, false] {
        let error = archive
            .restore(&target, true, dry_run)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("symlink path refused"), "{error}");
    }
    assert_eq!(
        fs::read_to_string(target.project.join(".haarness/skills/fmt/SKILL.md")).unwrap(),
        "team version"
    );
}

/// Comparing a file reached through a link never waits on a named pipe.
#[cfg(unix)]
#[test]
fn a_restore_never_waits_on_a_pipe_behind_a_link() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(&source.project, ".agents/skills/fmt/SKILL.md", "format");
    let archive = Archive::decode(
        &Archive::capture(&source, "pipe", Scope::Local)
            .unwrap()
            .bytes,
    )
    .unwrap();
    let target = fixture.roots("target");
    fs::create_dir_all(target.project.join(".haarness/skills/fmt")).unwrap();
    nix::unistd::mkfifo(
        &target.project.join(".haarness/skills/fmt/SKILL.md"),
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .unwrap();
    fs::create_dir_all(target.project.join(".agents/skills")).unwrap();
    symlink(
        "../../.haarness/skills/fmt",
        target.project.join(".agents/skills/fmt"),
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let roots = target.clone();
    std::thread::spawn(move || {
        let _ = sender.send(archive.restore(&roots, true, true).map(|_| ()));
    });
    let error = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the restore waited on the pipe")
        .unwrap_err()
        .to_string();
    assert!(error.contains("symlink path refused"), "{error}");
}

#[test]
fn installed_plugins_are_bundled_with_disabled_state_without_absolute_registry_paths() {
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    let plugin = fixture.0.join("external-plugin");
    put(
        &plugin,
        ".claude-plugin/plugin.json",
        "{\"name\":\"portable\"}",
    );
    put(&plugin, "commands/check.md", "check");
    put(
        &source.data,
        "plugins/installed.json",
        &serde_json::json!({"portable":{"path":plugin,"enabled":false,"marketplace":"local"}})
            .to_string(),
    );
    let archive = Archive::decode(
        &Archive::capture(&source, "plugins", Scope::Global)
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(archive.files.len(), 3);
    assert!(
        archive
            .files
            .keys()
            .any(|key| key.ends_with("/.haarness-disabled"))
    );
    assert!(
        !serde_json::to_string(&archive.manifest)
            .unwrap()
            .contains(fixture.0.to_str().unwrap())
    );
    let destination = fixture.roots("destination");
    archive.restore(&destination, false, false).unwrap();
}

#[test]
fn global_plugin_bundles_in_the_configuration_folder_are_shared() {
    // Marketplace plugins and MCP servers installed for all projects live in
    // `<config>/plugins/local`; a saved profile used to leave them out.
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(
        &source.config,
        "plugins/local/mcp-docs/.claude-plugin/plugin.json",
        "{\"name\":\"mcp-docs\"}",
    );
    put(
        &source.config,
        "plugins/local/mcp-docs/.mcp.json",
        "{\"mcpServers\":{\"docs\":{\"type\":\"remote\",\"url\":\"https://example.invalid/mcp\"}}}",
    );
    put(
        &source.config,
        "plugins/local/mcp-docs/.haarness-disabled",
        "",
    );
    put(&source.config, "plugins/installed.json", "{}");
    let archive = Archive::decode(
        &Archive::capture(&source, "plugins", Scope::Global)
            .unwrap()
            .bytes,
    )
    .unwrap();
    let mut keys = archive.files.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        [
            "global/config/plugins/local/mcp-docs/.claude-plugin/plugin.json",
            "global/config/plugins/local/mcp-docs/.haarness-disabled",
            "global/config/plugins/local/mcp-docs/.mcp.json",
        ]
    );
    let destination = fixture.roots("destination");
    archive.restore(&destination, false, false).unwrap();
    assert!(
        destination
            .config
            .join("plugins/local/mcp-docs/.mcp.json")
            .is_file()
    );
}

#[test]
fn catalog_is_persistent_searchable_immutable_and_validates_before_storage() {
    let fixture = Fixture::new();
    let roots = fixture.roots("source");
    put(&roots.config, "AGENTS.md", "instructions");
    let bytes = Archive::capture(&roots, "Engineering setup", Scope::Global)
        .unwrap()
        .bytes;
    let catalog = Catalog::new(&fixture.0.join("cloud"));
    assert!(catalog.put(b"invalid").is_err());
    let first = catalog.put(&bytes).unwrap();
    let second = catalog.put(&bytes).unwrap();
    assert_ne!(first.id, second.id);
    let catalog = Catalog::new(&fixture.0.join("cloud"));
    assert_eq!(catalog.list("ENGINEERING", 0, 1).unwrap()[0].id, second.id);
    assert_eq!(catalog.list("engineering", 1, 1).unwrap()[0].id, first.id);
    assert!(catalog.list("missing", 0, 10).unwrap().is_empty());
    assert_eq!(catalog.download(first.id).unwrap(), bytes);
    catalog.remove(first.id).unwrap();
    assert!(catalog.get(first.id).is_err());
    assert_eq!(catalog.list("", 0, 100).unwrap().len(), 1);
}

#[test]
fn shared_os_config_and_data_directory_exports_skills_once() {
    let fixture = Fixture::new();
    let mut source = fixture.roots("source");
    source.config = source.data.clone();
    put(&source.config, "haarness.json", "{}");
    put(&source.data, "skills/a/SKILL.md", "portable skill");
    let archive = Archive::decode(
        &Archive::capture(&source, "shared roots", Scope::Global)
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(archive.files.len(), 2);
    let mut destination = fixture.roots("destination");
    destination.config = destination.data.clone();
    archive.restore(&destination, false, false).unwrap();
    assert_eq!(
        fs::read_to_string(destination.data.join("skills/a/SKILL.md")).unwrap(),
        "portable skill"
    );
}

#[test]
fn runtime_configuration_read_lock_prevents_partial_restore_visibility() {
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    let destination = fixture.roots("destination");
    put(&source.config, "AGENTS.md", "updated");
    let archive = Archive::decode(
        &Archive::capture(&source, "locking", Scope::Global)
            .unwrap()
            .bytes,
    )
    .unwrap();
    let guard = configuration_lock(&destination.data).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let target = destination.clone();
    let thread = std::thread::spawn(move || {
        archive.restore(&target, false, false).unwrap();
        sender.send(()).unwrap();
    });
    assert!(
        receiver
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err()
    );
    assert!(!destination.config.join("AGENTS.md").exists());
    drop(guard);
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    thread.join().unwrap();
    assert_eq!(
        fs::read_to_string(destination.config.join("AGENTS.md")).unwrap(),
        "updated"
    );
}

#[test]
fn case_collisions_and_file_directory_overlap_are_rejected_before_restore() {
    for paths in [
        [
            "project/.haarness/agents/a.md",
            "project/.haarness/agents/A.md",
        ],
        [
            "project/.haarness/skills/a",
            "project/.haarness/skills/a/SKILL.md",
        ],
    ] {
        let manifest = Manifest {
            format: "haarness.profile".into(),
            version: 1,
            name: "collision".into(),
            scope: Scope::Local,
            created_at: Utc::now(),
            entries: paths
                .iter()
                .map(|path| Entry {
                    path: (*path).into(),
                    size: 1,
                    sha256: hash(b"x"),
                    executable: false,
                })
                .collect(),
        };
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        for path in paths {
            zip.start_file(path, options).unwrap();
            zip.write_all(b"x").unwrap();
        }
        assert!(Archive::decode(&zip.finish().unwrap().into_inner()).is_err());
    }
}

#[test]
fn thousands_of_small_resources_roundtrip_without_an_entry_count_cap() {
    let fixture = Fixture::new();
    let source = fixture.roots("many-source");
    for index in 0..4100 {
        put(
            &source.config,
            &format!("commands/command-{index:05}.md"),
            "instruction",
        );
    }
    let bytes = Archive::capture(&source, "Large library", Scope::Global)
        .unwrap()
        .bytes;
    let archive = Archive::decode(&bytes).unwrap();
    assert_eq!(archive.manifest.entries.len(), 4100);
    let target = fixture.roots("many-target");
    archive.restore(&target, false, false).unwrap();
    assert_eq!(
        fs::read_to_string(target.config.join("commands/command-04099.md")).unwrap(),
        "instruction"
    );
}

#[test]
fn migrated_resources_export_identical_copies_once_but_preserve_different_legacy_content() {
    let fixture = Fixture::new();
    let roots = fixture.roots("migrated");
    put(&roots.config, "skills/same/SKILL.md", "current");
    put(&roots.data, "skills/same/SKILL.md", "current");
    put(&roots.config, "skills/different/SKILL.md", "current");
    put(&roots.data, "skills/different/SKILL.md", "older");
    let archive = Archive::decode(
        &Archive::capture(&roots, "Migrated", Scope::Global)
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(archive.files.len(), 3);
    assert!(
        archive
            .files
            .contains_key("global/config/skills/same/SKILL.md")
    );
    assert!(
        !archive
            .files
            .contains_key("global/data/skills/same/SKILL.md")
    );
    assert!(
        archive
            .files
            .contains_key("global/data/skills/different/SKILL.md")
    );
}
