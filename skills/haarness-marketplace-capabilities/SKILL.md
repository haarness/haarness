---
name: haarness-marketplace-capabilities
description: Test every Haarness marketplace item type and live runtime load/unload across Cloud, CLI, tools, SDK and Desktop, including private workspace visibility and public review.
---

# Marketplace and runtime capabilities

Read `haarness-cloud/docs/reviews/marketplace-live-2026-09-27.md` and `docs/WORKSPACE-SETUP-VERIFICATION-2026-09-27.md` relative to Cloud. Those are historical evidence, not a substitute for rerunning changed surfaces. Read script help/source before choosing fixture/live settings.

Run Cloud `scripts/test-marketplace-live.py`, `scripts/test-workspace-setup-live.py`, `scripts/test-cloud-plugins-live.py`, and native `scripts/test-marketplace.py` / `scripts/test-real-plugins.sh` as applicable. Use a disposable account/project/session; preserve existing installations.

Cover skills, agents, MCP servers, Claude Code plugins and extensions: list/search/detail, submission/upload, install, update, enable/disable, uninstall and failure recovery. Private visibility follows membership in the owning Cloud account (the commercial team/workspace), independently of individual runtime sessions. A link or enterprise AD group does not grant access. Test another account cannot see private metadata or download its artifact. Public items require the supported review/publication flow; test review state and artifact access independently.

Use Chrome management pages and the runtime Capabilities pane to install from the marketplace into a running session. Prove the next LLM turn can use it without restarting the session. Unload it and verify its tools/skill/agent no longer appear; reattach and verify it works again. Inspect durable `.haarness` configuration and SDK/API readback. Preserve independent user config and shared plugin-owned resources on uninstall. Test malformed manifests, duplicates, partial install rollback and revision conflicts.

For MCP, consult current official docs before selecting a public endpoint. Previously used examples: Microsoft Learn, Cloudflare docs and DeepWiki. An actual successful tool call with a cited result is stronger than a `connected` badge. For Claude Code plugins, inspect the current official plugin manifest and test bundled skills, agents and MCP configuration; exclude Apple-only integrations. Never copy private workspace content into a public MCP request merely to test connectivity.

Use varied LLM prompts to select a skill, call an MCP tool, and delegate through an installed agent. Verify real tool traces and outputs, not prose claims. Check CLI/SDK parity and use Desktop Electron tests for native installation. For browser file-picker failures, report the exact unverified UI step separately from a successful binary upload API test.

Record installed IDs, artifact versions/hashes, session/chat IDs, actual tool names and output proof. Remove only test submissions/installs/resources using normal reversible lifecycle operations.
