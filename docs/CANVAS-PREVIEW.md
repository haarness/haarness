# Canvas and file previews

How the `preview` tool, Desktop's sandboxed HTML previews and their saved versions work, with the API, SDK methods and tests behind them. For contributors and for client authors who show previews.

> **User guide:** [Preview pages and use the browser](https://haarness.ai/docs/desktop/previews-and-browser).

Ask Haarness Desktop to create a dashboard, demo or report inside your project. The agent calls `preview` after writing the file, and the Files panel opens beside your conversation. It refreshes when the agent calls `preview` again.

Try: “Create an interactive sales dashboard in `sales.html` using embedded sample data and inline JavaScript, then preview it.”

- HTML and SVG open as previews. Use **Code** to edit and **Preview** to run the saved or unsaved document. Opening Markdown from the file tree retains the existing editor default; the preview tool can show rendered Markdown explicitly.
- Each preview banner shows the exact version recorded at that point. Expand **Code** or **Preview**, or use **Open preview** to put that version in the panel.
- Earlier banners offer **Compare** and **Restore**. Restore backs up the current bytes first; **Undo restore** recovers that backup. A pinned panel also offers **Show current** and **Restore this version**. Stale restore requests fail rather than overwrite concurrent changes. A file whose folder was removed shows as deleted, and restoring it makes the folder again (only inside the workspace and outside `.git`).
- Unsaved editor drafts are retained. An automatic preview refresh asks you to save or discard the draft before replacing its tab. Reopening a conversation does not reopen its old previews.

HTML runs in opaque sandboxed frames, without the app's storage, Electron bridge, network APIs, popups, form submissions or external navigation. Inline JavaScript and CSS work. Libraries can load from `https://unpkg.com`; jsDelivr npm, cdnjs and D3 URLs are rewritten to that public CDN. Check library URLs because package layouts can differ. Embed images/data and use system fonts. Navigation attempts can leave Chromium's blocked page; the panel reports the failure without automatically rerunning the page.

`preview` accepts `filePath` (workspace-relative or absolute inside the workspace), optional `mode` (`preview` or `code`) and `title`. It supports HTML, Markdown, SVG and common images, with a 5 MiB version limit. Files outside the workspace and symlinks escaping it are rejected. Restoring symlinks is unsupported. Versions live in Haarness's data directory (`<data dir>/data/preview` with `--data-dir` or `HAARNESS_DATA_DIR`), scoped to the canonical workspace, with the newest 500 retained; a cargo test binary keeps them in a folder of its own under the system temp folder, never in the home of the person running it. They do not modify project Git history. Older, pruned versions are shown as unavailable.

Desktop opts in with `tools.preview=true` on turns and commands; queued input preserves its `preview` capability through storage and restart. Clients that omit it retain their existing tool catalog and prompt. The server advertises `files.preview.v1`. TypeScript and Python SDKs expose `previewVersion`/`preview_version`, `currentPreview`/`current_preview` and `restorePreview`/`restore_preview` for authenticated, workspace-scoped version access.

The Canvas behavior and sandbox are adapted from the earlier design and its fixes. The npm CDN replaces the internal mirror. The separate internal email service is not part of this port. The ported helper's MIT notice ships with Desktop as `upstream-preview-license.txt`.

## Verification

From the repository root, after following `.skills/haarness-build-run/SKILL.md`:

```sh
cargo build -p haarness-cli
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-canvas-preview.py
npm --prefix apps/desktop run build
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'canvas preview'
# Include the real public-CDN integration check when network access is available:
HAARNESS_LIVE_PREVIEW_CDN=1 HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'canvas preview'
```

The API fixture exercises the actual server and deterministic provider. Electron tests exercise tool results over the event stream, executable previews, Code/Preview, immutable history, comparison, restore/undo, unsaved drafts, history replay, and a local HTTP receiver proving blocked requests are never sent.

### Validation record — 2026-10-03

The port was checked against unchanged `main` at `46f44e7ee2b099ac4caa04a1a0748cd78de17d64`, using separately built baseline and candidate binaries with isolated app/server data.

- Rust library tests: 272 passed. Server API integration tests: 120 passed.
- Desktop unit tests: 153 passed. TypeScript SDK: 20 passed. Python SDK: 30 passed. Desktop production build and typecheck passed.
- The actual CLI/server preview fixture and live OpenAPI/native SDK generation checks passed. The combined native/Cloud generator has existing sibling Cloud snapshot drift; native outputs were checked independently.
- Final Electron acceptance: 21 passed, including all editor audits, overlapping preview requests, chat switching, actual write/edit/preview tool calls, external file changes, restore/undo, blocked network/navigation, Markdown, replay, and live public-CDN loading. The rendered result was visually inspected.

The broad Electron run recorded 182 passes, 31 skipped cases and 12 cases blocked by serial-suite failures. A Canvas fixture initially inherited a different project's provider; its setup now explicitly selects its project, and it passes after the preceding Markdown recovery scenario. Of the other 13 failures, ten reproduced on unchanged `main`, while three passed on both baseline and candidate reruns. The final focused regression comparison recorded 26 passes and seven failures, all matching baseline: plugin-folder selection, three manual-compaction scenarios, remote tool approval and the local/remote definition editors. Browser folder-approval controls also fail on baseline with their preceding scenarios. The repository-wide suite is therefore not entirely green; no new regression was detected in the covered paths.
