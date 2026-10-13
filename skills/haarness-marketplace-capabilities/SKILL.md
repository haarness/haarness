---
name: haarness-marketplace-capabilities
description: Test every Haarness marketplace item type and live runtime load/unload across Cloud, CLI, tools, SDK and Desktop, including private workspace visibility and public review.
---

# Marketplace and runtime capabilities

Read `haarness-cloud/docs/reviews/marketplace-live-2026-09-27.md` and `docs/WORKSPACE-SETUP-VERIFICATION-2026-09-27.md` relative to Cloud. Those are historical evidence, not a substitute for rerunning changed surfaces. Read script help/source before choosing fixture/live settings.

Run Cloud `scripts/test-marketplace-live.py`, `scripts/test-workspace-setup-live.py`, `scripts/test-cloud-plugins-live.py`, and native `scripts/test-marketplace.py` / `scripts/test-real-plugins.sh` as applicable. Use a disposable account/project/session; preserve existing installations.

Cover skills, agents, MCP servers, Claude Code plugins and extensions: list/search/detail, submission/upload, install, update, enable/disable, uninstall and failure recovery. Private visibility follows membership in the owning Cloud account (the commercial team/workspace), independently of individual runtime sessions. A shared link does not grant access. Test another account cannot see private metadata or download its artifact. Public items require the supported review/publication flow; test review state and artifact access independently.

Use Chrome management pages and the runtime Capabilities pane to install from the marketplace into a running session. Prove the next LLM turn can use it without restarting the session. Unload it and verify its tools/skill/agent no longer appear; reattach and verify it works again. Inspect durable `.haarness` configuration and SDK/API readback. Preserve independent user config and shared plugin-owned resources on uninstall. Test malformed manifests, duplicates, partial install rollback and revision conflicts.

For MCP, consult current official docs before selecting a public endpoint. Previously used examples: Microsoft Learn, Cloudflare docs and DeepWiki. An actual successful tool call with a cited result is stronger than a `connected` badge. For Claude Code plugins, inspect the current official plugin manifest and test bundled skills, agents and MCP configuration; exclude Apple-only integrations. Never copy private workspace content into a public MCP request merely to test connectivity.

For storage changes, run Cloud's opt-in `cargo test -p cloud-worker --test marketplace_r2 -- --ignored --nocapture`
with the scoped development R2 settings, a disposable `HAARNESS_CLOUD_TEST_DATABASE_URL`, and a freshly built
`HAARNESS_BIN`. Set `HAARNESS_TEST_PYTHON` to Python 3.10+ with the native Python SDK dependencies, and
`PYTHONPATH` to the native repository's `sdks/python/src`. The fixture launches the API and validation worker,
invokes `scripts/test-marketplace-r2.py`, and cleans its own R2 prefix and database schema. The script covers
ZIP upload/deduplication, tenant isolation, signed download hashes, Git imports, MCP support files, and native
installation/live update/removal through CLI and SDK. It disables its synthetic npm registry after publishing
two versions so successful extension installs prove the tarballs come from R2. Its model is deterministic;
run the separate live LLM flow when prompt/tool selection is part of the change.

Verify signed links remain intact in both API-key and Cloud generation-token modes. ZIP links are in the
manifest's `artifact`; npm tarball links are in `install.extension.artifact`, preserving older ZIP-only clients.
Metadata and MCP templates remain in PostgreSQL; installed runtime files remain local. Neither is a missing
R2 blob. Transitive npm dependencies still use the package manager. Reject wrong hashes and external registry
tarball origins, and simulate a storage outage to verify the submission can recover on retry. For MCP ZIPs,
require one server in root `.mcp.json` and call a tool whose script was downloaded in that ZIP. Test both
flat and single-folder ZIP layouts: `${CLAUDE_PLUGIN_ROOT}` must resolve beside the original `.mcp.json`.

On macOS, if Rust's native certificate enumeration fails with `UnknownIssuer` while the system HTTPS client
validates the same endpoint, an explicit trusted system CA bundle (`SSL_CERT_FILE=/etc/ssl/cert.pem`) can be
used for the local test process. Keep TLS verification enabled. Record the override and verify cleanup even
after a failed run; never sweep prefixes or database schemas belonging to other tests.

Use varied LLM prompts to select a skill, call an MCP tool, and delegate through an installed agent. Verify real tool traces and outputs, not prose claims. Check CLI/SDK parity and use Desktop Electron tests for native installation. For browser file-picker failures, report the exact unverified UI step separately from a successful binary upload API test.

Record installed IDs, artifact versions/hashes, session/chat IDs, actual tool names and output proof. Remove only test submissions/installs/resources using normal reversible lifecycle operations.

## Validated 2026-10-13 (every surface, real models)

A private Cloud stack (own Postgres container, ports outside 18xxx) with two fixture accounts covered CLI,
TUI in tmux, Desktop through Playwright `_electron` over CDP, the model's tools via `haarness run --format json`
plus `/v1/threads/{id}/items` for the tool trace, and Cloud web plus a Cloud session in the browser. Sign the
browser in through the fixture IdP by adding `login_hint=<user>` to its authorize URL; complete a native
`haarness login --json --no-browser` link the same way with an HTTP client. Solid inputs need the native value
setter plus an `input` event when scripted. Copy a rebuilt binary with `rm` then `cp`: overwriting a signed
copy in place gets it SIGKILLed (exit 137).

Use trusted, read-only real resources: Anthropic's `skills` and `claude-plugins-official` repos (Git
submissions with a subdirectory), the Microsoft Learn and DeepWiki remote MCPs (a harmless header placeholder
proves per-install values), an owned npm extension from a tiny local registry
(`HAARNESS_CLOUD_MARKETPLACE_NPM_REGISTRY`). Avoid plugins with hooks and agents pinned to `model: opus`.

Checks that found real bugs and must stay covered:
- Claude marketplace entries pinned by `sha`/`ref` (and `url` + `path`) install exactly that commit; build
  a two-commit fixture repo and compare the installed file.
- `enable|disable|remove <publisher/slug>`, installed state (`enabled`) in Desktop/TUI/Cloud session lists.
- Extension uninstall/update/failed install leave no `data/extensions/packages/<id>`.
- A public submission is invisible to other accounts and anonymous visitors until approved
  (`POST /internal/ops/marketplace/versions/{id}/review`); a yank keeps 410-with-reason for pinned installers
  and the CLI prints that reason.
- Sessions of a non-production stack read that stack's catalog (`HAARNESS_CLOUD_MARKETPLACE_URL`).
- The model installs `name@marketplace` through `marketplace_manage` (`plugin_marketplaces`, `plugin`).
- `@agent` offers Claude-format agents (no `mode`) in the TUI; profiles carry `config/plugins/local`.

Open gaps: Desktop's plugin install form asks for plugin and marketplace names instead of listing a registered
marketplace's plugins; entries with `strict: false` and entry-level `skills` (no plugin.json) are not
installable; there is no native publish/share command (Cloud web and the SDK publish); marketplace installs run
without an approval prompt in Review mode, like other in-project actions.

## Explore beyond this guide

Treat the documented flows as a starting point. Do not hesitate to test relevant behavior that is not documented here: inspect the current implementation, follow unexpected results, and cover interactions and failure paths suggested by the task. Stay within the authorized environment and scope. Fix discovered bugs when authorized, verify the fixes, and record newly validated steps and remaining gaps so this skill improves with use.
