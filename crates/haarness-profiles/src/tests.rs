use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("haarness-profile-test-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
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
        let bytes = Archive::capture(&source, name, scope).unwrap();
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
    let archive =
        Archive::decode(&Archive::capture(&roots, "project", Scope::Local).unwrap()).unwrap();
    let names: Vec<_> = archive.files.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["project/.haarness/commands/visible.md"]);
}

#[test]
fn conflict_preflight_changes_nothing_and_force_replaces_only_listed_files() {
    let fixture = Fixture::new();
    let source = fixture.roots("source");
    put(&source.config, "AGENTS.md", "new instructions");
    put(&source.project, "haarness.json", "{}");
    let archive =
        Archive::decode(&Archive::capture(&source, "conflict", Scope::Both).unwrap()).unwrap();
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
    let archive =
        Archive::decode(&Archive::capture(&source, "links", Scope::Global).unwrap()).unwrap();
    let destination = fixture.roots("destination");
    let outside = fixture.0.join("outside");
    put(&outside, "review.md", "original");
    symlink(&outside, destination.config.join("agents")).unwrap();
    assert!(archive.restore(&destination, true, false).is_err());
    assert_eq!(
        fs::read_to_string(outside.join("review.md")).unwrap(),
        "original"
    );
    fs::remove_dir_all(source.config.join("agents")).unwrap();
    symlink(&outside, source.config.join("agents")).unwrap();
    assert!(Archive::capture(&source, "links", Scope::Global).is_err());
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
    let archive =
        Archive::decode(&Archive::capture(&source, "plugins", Scope::Global).unwrap()).unwrap();
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
fn catalog_is_persistent_searchable_immutable_and_validates_before_storage() {
    let fixture = Fixture::new();
    let roots = fixture.roots("source");
    put(&roots.config, "AGENTS.md", "instructions");
    let bytes = Archive::capture(&roots, "Engineering setup", Scope::Global).unwrap();
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
    let archive =
        Archive::decode(&Archive::capture(&source, "shared roots", Scope::Global).unwrap())
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
    let archive =
        Archive::decode(&Archive::capture(&source, "locking", Scope::Global).unwrap()).unwrap();
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
    let bytes = Archive::capture(&source, "Large library", Scope::Global).unwrap();
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
    let archive =
        Archive::decode(&Archive::capture(&roots, "Migrated", Scope::Global).unwrap()).unwrap();
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
