# Portable configuration profiles

What a configuration profile archive holds and leaves out, how save and restore check it, the self-hosted profile catalog and its API. For developers working on profiles and for operators running a catalog server.

> **User guide:** [Save and restore your setup](https://haarness.ai/docs/cli/configuration#save-and-restore-your-setup).

Profiles move Haarness configuration between installations and projects. Each
ZIP archive has a versioned manifest and SHA-256 checksums for every file.
Profile commands do not start models, register workspaces, or load credentials.

## Save and restore

```sh
haarness profile save engineering --output ./engineering.profile.zip
haarness profile inspect ./engineering.profile.zip
haarness --workspace /path/to/project profile restore ./engineering.profile.zip --dry-run
haarness --workspace /path/to/project profile restore ./engineering.profile.zip
# Explicitly replace existing files; unrelated files remain intact.
haarness profile restore ./engineering.profile.zip --force
haarness profile save global-setup --scope global
haarness profile save project-setup --scope local
```

Scope defaults to `both`. Global targets use the current OS config/data
directories; `--data-dir` selects an isolated installation. Project targets are
relative to the exact `--workspace` directory (current directory by default),
without automatically selecting a parent Git root. Commands print a short
summary; `--json` prints the data as JSON. Without `--output`, save writes
`<name>.profile.zip` in the current directory. Existing output files and
restore targets require `--force` to overwrite; `restore --dry-run` lists every
file it would create or overwrite. The usage-statistics choice (`telemetry` in
the global `haarness.json`) belongs to each computer: save leaves it out and
restore keeps the target's own.

Desktop's Settings → **Profiles** runs the same commands: **Save profile** asks
for a name and scope, **Restore profile** picks an archive, previews it with
`--dry-run` and lists the files it would replace before you confirm, and
**Manage catalog** reaches a catalog server (below). In the TUI, `/profiles`
(or `/profile`) works with the profile catalog of the server it is attached to
(`<data>/profiles`): **Save profile** captures a name and scope into it, and
each saved profile shows its files with **Restore profile** (listing the files
it will replace before **Replace configuration files**) and **Delete profile**.

## Included content and portability

- Global TOML/JSON/JSONC configuration (`config.toml`, `haarness.json{,c}`,
  `opencode.json{,c}`, `extensions.json`, `tui.json{,c}`, Claude-style
  `settings.json`), `AGENTS.md`, custom agents, commands, hooks, workflows,
  output styles, themes, extensions and skills (the `CONFIG_FILES` and
  `CONFIG_DIRS` lists in `crates/haarness-profiles/src/lib.rs`).
- Global data-directory skills and local plugin bundles.
- Haarness-installed plugins, copied as standalone local bundles with their
  enabled/disabled state preserved. Machine-specific cache/marketplace
  registries are not copied. Reinstall from the marketplace to resume upstream
  updates; restored bundles can be edited and enabled/disabled locally.
- Project configuration and instructions in the workspace root and supported
  `.haarness`, `.agents`, and supported legacy asset directories, plus
  project-local Haarness plugins. Unrelated project files are excluded.

Profiles exclude credential stores (`auth.json`, MCP OAuth state), `.env`
files, PEM/key files, Git metadata, dependencies, conversations, databases,
logs, runtime discovery, workspace registrations, and scheduler state.
Restoring never registers scheduled jobs. External home configuration owned
by other applications is not captured. Anything else in the configuration
folder is left out too, notably [prompt files](PROMPT-FILES.md) (`prompts/`),
the terminal UI's own state and history (`tui-state.toml` and the others) and
the switches Settings keeps in `capabilities.json`.

**Configuration and scripts can contain inline secrets.** Archives retain
their contents, including literal environment/header values, and are neither
encrypted nor automatically sanitized. Inspect included files before sharing.
Environment and keychain references (`${VAR}`, `${keychain:NAME}`) are
preserved without expansion, and keychain entries themselves are never
exported: a profile whose MCP settings name `${keychain:github-token}` carries
no secret, so store it again on the new machine with
`haarness mcp secret set github-token`. Reconnect provider accounts and MCP
OAuth after restore. Embedded absolute paths may need editing
on another machine; plugin-relative placeholders resolve at the restored root.

Restore validates the entire archive and all targets before changing files,
stages replacements, and rolls back applied replacements if an operation fails.
It rejects symlinks, traversal, unexpected files, duplicate/case-colliding paths,
checksum failures, unsupported versions, and non-regular entries. On Unix,
archives and restored files are owner-only; scripts retain owner execute
permission. Limits: 32 MiB ZIP, 64 MiB expanded, 2 MiB manifest; there is no
separate cap on the number of files. Rollback handles operation failures, not
a machine crash during a multi-file restore.

Running servers use normal idle-time configuration reload. Project configuration loads by default and can be excluded with
`--disable-project-config` or `HAARNESS_DISABLE_PROJECT_CONFIG=1`. Restore archives you trust:
configuration can define tools, hooks, and workflows.

## Profiles and a base folder

A profile is a snapshot: restoring it copies files into the person's own global
and project configuration, which they then own and change. A
[base folder](CONFIGURATION-SCOPES.md#base-folder) (`HAARNESS_BASE_CONFIG_DIR`)
is a live, read-only layer below that configuration, shared by everyone whose
Haarness starts with the variable and updated where it lives (a `git pull`, a
device management push). Saving a profile never includes the base folder,
restoring one never writes into it, and whatever a profile restores overrides
the base folder's settings and definitions of the same name. Ship team defaults
that should stay current as a base folder; hand someone a setup to make their
own as a profile.

## Cloud storage and discovery

Any current Haarness server provides an authenticated profile catalog. Run
`haarness serve` with persistent storage on your own host and expose it through
HTTPS for remote clients. A local setup uses:

```sh
# Set HAARNESS_SERVER_TOKEN to a private token before starting the server.
haarness --data-dir /path/to/catalog-data --workspace /path/to/catalog-workspace serve --port 4096

# In another terminal, use the same bearer token.
export HAARNESS_PROFILE_URL=http://127.0.0.1:4096
export HAARNESS_PROFILE_TOKEN="$HAARNESS_SERVER_TOKEN"
haarness profile cloud-push ./engineering.profile.zip
haarness profile cloud-list --query engineering --limit 20
haarness profile cloud-get PROFILE_ID
haarness profile cloud-download PROFILE_ID --output ./downloaded.profile.zip
haarness profile cloud-restore PROFILE_ID --dry-run
haarness profile cloud-restore PROFILE_ID
haarness profile cloud-delete PROFILE_ID
haarness profile save engineering --cloud
```

`--url` and `--token` override environment settings. URLs accept a server root
or `/v1` base, including reverse-proxy prefixes. HTTP is allowed only on exact
loopback hosts; remote servers require HTTPS. Redirects are refused, tokens
are omitted from errors, and downloads stay on the selected endpoint.

The catalog lives in `<data>/profiles`, shared by servers using that data root.
Every upload has a new immutable UUID and never changes server configuration.
Discovery matches names case-insensitively, newest first, with `--offset` and
`--limit` pagination (1–100). Storage allows at most 1,000 archives; delete old
IDs to reclaim space.

This is a self-hosted catalog using the server's existing bearer token. Every
authorized server client can read, upload, and delete profiles. It does not
provide anonymous public downloads, per-user/group sharing, or a hosted account
service. A server token is not a public sharing link.

## API

All operations require existing server bearer-token and Host/Origin checks.
They are included in `/openapi.json`.

| Method | Path | Result |
| --- | --- | --- |
| POST | `/v1/profiles` | Raw `application/zip`; 201 with metadata |
| GET | `/v1/profiles?query=...&offset=0&limit=100` | Metadata array |
| GET | `/v1/profiles/{id}` | Metadata including ZIP SHA-256 |
| GET | `/v1/profiles/{id}/download` | Original ZIP bytes |
| DELETE | `/v1/profiles/{id}` | 204; subsequent reads return 404 |

## Verification

```sh
cargo test -p haarness-profiles
cargo test -p haarness-cli --test profiles_e2e
```

The integration tests invoke the real CLI against isolated installations,
restore and discover a custom agent, run a test-provider turn, and exercise an
authenticated HTTP server through upload/discovery/download/restore, restart
persistence, authentication/Origin rejection, and deletion.
