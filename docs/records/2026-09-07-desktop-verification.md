# Desktop verification — 2026-09-07

## Dismiss every modal from its backdrop — 2026-09-23

The Haarness account dialog now uses the shared backdrop handler. All ten native
dialog implementations were audited; pending loads and saves no longer block
outside clicks or the close controls. A click dismisses only the topmost dialog.
Panel padding, clicks inside, and drags across the panel boundary keep it open.
Closing a submitted form lets its existing operation finish; guards prevent a
late result from closing a replacement form. Provider login dismissal also
cancels a sign-in that returns after the dialog has gone away.

Passed Desktop typecheck, production build, 147 unit tests, and ten real Electron
scenarios with isolated state. Coverage includes every native dialog
implementation, nested dialogs, focus restoration, unsubmitted drafts, pending
file/provider saves followed by replacement dialogs, and account dismissal both
signed in and during browser sign-in. The account check used the local Cloud
stack and fixture identity provider, including model discovery and logout.
Existing MCP, schedule execution, custom provider, memory conflict/delete, and
self-hosted router workflows also passed. The signed-in account screenshot was
inspected. The running development Desktop received the renderer edits through
hot reload; its account profile and the user's servers were preserved.

Evidence: `/tmp/haarness-modal-e2e`, `/tmp/haarness-modal-save-regressions`, and the
corresponding `.log` files; build/unit logs are `/tmp/haarness-modal-build.log`
and `/tmp/haarness-modal-unit.log`.

## Multiple projects on one remote server — 2026-09-11

Implemented authenticated host folder discovery and routing to independently leased directory daemons. Desktop retains one saved server credential and separate project identities, including the legacy default project.

The final development build passed five real Electron/server scenarios together (27.7 seconds): remote multi-project browsing/opening and isolation, local project trust/reconnect, multiple-project navigation, terminal lifecycle, and remote file context. The new scenario checks invalid-path recovery, Unicode/space paths, live replies, isolated history and files, terminal working directory and reattachment, drafts across project/server switching and reload, pending questions, known-project discovery, and forgetting a project without removing its server/history. Missing bearer credentials and unrelated browser origins are rejected. Two existing test synchronization assumptions were repaired: reading during reconnect and inheriting a collapsed folder from an earlier scenario.

All 82 desktop unit tests and 68 server library tests passed; the CLI build and desktop typecheck/production build passed. Unit checks also cover older-server upgrade messages and refusing credential-bearing redirects. E2E used real HTTP, SSE, and WebSockets through the remote gateway on loopback, with private temporary daemons and the deterministic provider; it does not claim validation against an external machine or TLS ingress. Screenshots of the project browser and grouped remote projects were inspected.

Evidence: `/tmp/haarness-remote-verified-e2e` and `/tmp/haarness-remote-verified-e2e.log`; unit, Rust, and build logs use `/tmp/haarness-remote-*.log`. Test workspaces and their daemons were cleaned up. A remote host must run the updated `haarness serve` to offer project browsing.

The Electron desktop lives in `apps/desktop` and uses the per-directory Haarness runtime. This verification used macOS 26.5.1 on Apple silicon, Electron 44.2.0, Bun 1.3.14, and private temporary Git workspaces with the deterministic echo provider. No paid provider calls or personal runtime storage were used by the automated tests.

## Executed desktop scenarios

The current desktop renderer passed all thirty-nine Playwright Electron scenarios in the packaged macOS app, using its bundled runtime for both Electron and the independent SDK client. Final runtime rebuild checks are recorded below. The scrollbar's focused validation is recorded below. Earlier revision evidence also includes compatibility checks with previous runtimes:

1. Create a session, stream Markdown, edit and save a real file, inspect split/unified diffs, add a line comment to the chat draft, type into a real terminal, and verify its output after reattachment.
2. Observe text while streaming, queue guidance, resolve a permission, answer a question, and interrupt ongoing work. Verify the single Stop/Send position, gently pulsing composer border, reduced-motion behavior, and absence of the extra Working row and send-key footer hint.
3. Create and update a session from an independent client, reload the Electron window without duplicating items, restart the private daemon, reconnect, and continue the same conversation.
4. Change appearance, inspect the settings catalogs, verify that raw JSON-only settings are absent, install and disable a local plugin, install/enable/validate an extension, and execute the extension tool through a conversation.
5. Execute a workflow and create/pause/clear a goal.
6. Preserve an unsaved file across panel navigation, reject a conflicting external edit, require explicit discard, reject traversal/Git metadata reads, and verify renderer Node access is unavailable.
7. Create and edit project agents, commands and skills, preserve metadata such as tool ceilings and scope, wait for native configuration reload, and execute the updated slash command.
8. Create, edit and remove a disabled schedule.
9. Keep chat visible beside files and review; preserve an unsaved draft while switching tabs; resize panes by mouse and keyboard; expand/collapse panes; use a 900 px window without horizontal overflow; reload and restore saved dimensions.
10. Select all eight palettes in both Light and Dark, check live editor/syntax/terminal colors, follow system appearance with Auto, and preserve the selected palette and appearance through a window reload.
11. Keep the sidebar limited to conversations/projects and settings, omit the secondary Compact/Undo/Redo/export toolbar, reach secondary features from the Haarness menu, and use long slash commands at 1420, 1100, and 900 px without widening the chat. Verify keyboard completion/dismissal, pointer selection, and suggestions beside an open file panel.
12. Close Electron during a pending permission, verify the daemon and turn remain alive, then resolve and finish the work through the independent client.
13. Open the native folder picker directly, enable project configuration by default, upgrade an idle daemon that had project configuration disabled, retain conversation history, preserve the connection on picker cancellation, reject a setting change during active work, and retain an explicitly disabled setting when reopening a recent workspace.

14. Surface a real missing-provider-credential error in the current chat within five seconds, without navigating, reload without duplicating the alert, and recover in the same conversation with the deterministic provider.
15. Add and switch between projects without changing their order; reorder projects and sessions using real dragging and keyboard controls; preserve focus and positions through activity, renaming, project switching, and window reloads.
16. Open Files, Review, and a working Terminal through the side-panel launcher; reorder panel and terminal tabs with keyboard controls and retain terminal positions across reloads.
17. Reorder conversation tabs with horizontal keyboard controls and restore the order when reopening them after reload.
18. Keep multiple projects expanded with independent disclosure controls; retain the active conversation and draft while expanding another project; observe inactive-project creation, renaming, activity and daemon restart through SSE; select an exact conversation across projects; and restore expanded/collapsed choices after reload.
19. Display forks in the ordinary project conversation list; rename, archive and restore them; observe forks created in an inactive project; retain their visibility after switching projects and reloading.
20. Drag whole project/session rows through their centres in both directions, including collapsed projects. Click project titles to expand and collapse them; verify the same behavior using Enter and Space and restore the collapsed state after reload. Conversation lists have no filter input, including projects with more than five conversations. Use Alt+arrow/Home/End ordering without grips, move menus, or notifications, retain focus and positions after reload, and archive through the single row action. Archiving an inactive project’s conversation leaves the active chat unchanged. Settings action menus remain anchored to their controls.
21. Show a project-aware new-conversation heading and four starter cards; adapt the cards to narrow windows; verify Light and Dark screenshots; keep starter prompts editable instead of sending automatically. Selecting a model or agent does not create an empty conversation; sending the first message does.
22. Scroll upward by a small wheel movement while output streams and retain the reading position through more tokens, Markdown reflow and completion; resume following through Jump to latest or returning to the bottom; exercise keyboard scrolling as well.
23. Create and edit global/project resources through desktop management without crossing scopes; preserve global credential storage and private file permissions; install and disable a project extension independently of global extension settings.
24. Reopen a project whose earlier executable still owns a permission-waiting task; keep its daemon and permission intact; use another conversation and another project; reload; answer the fixture permission; then replace the idle daemon through concurrent reopen requests without losing history.
25. Delay one project's startup while navigating elsewhere; preserve its cached conversation index and separate drafts; honor New chat during startup; avoid stale navigation taking over the current view; submit the retained draft into the correct project.
26. Attach an image to the first message, verify the image in the transcript, and start another chat without the sent image leaking into its draft.
27. Create a real Git worktree from the composer, group its conversations under the same project, preserve the new-chat draft, default another chat to the project folder, keep approvals independent, and route files, review, and imported session IDs to the exact checkout.
28. Edit and snapshot a Git project containing 10,005 files, list the entire directory, and open the last file through the explorer.

29. Use themed pickers for models, agents, effort, review, settings, and native dialog forms. Search long lists, handle empty results, choose with keyboard navigation and type-ahead, reopen with fresh type-ahead state, dismiss with Escape/Tab/outside click, retain Settings on picker dismissal, and verify viewport bounds and absence of native HTML selects.

30. Search and choose projects directly from the welcome heading; mark the selection, keep actions reachable with empty results, cancel and complete New project, preserve separate drafts, and create/reload general chats outside projects without inheriting their commands or history.
31. Show compact assistant timestamp/Copy/Fork footers on hover and focus, copy the original Markdown into the real clipboard, and fork through the selected reply while excluding later replies and preserving the original conversation. User messages offer Copy only, verified through keyboard activation and the real clipboard.
32. Probe the compiled native speech helper, inject native-style partial/final speech through a subprocess and real IPC, insert at the text selection without duplicating partials, stop on manual typing/conversation switching/reload, handle denied access without a blocking dialog, and exercise the dictation keyboard shortcut. The speech fixture does not record microphone audio.

33. Launch two delegated agents, show one description/agent/model/status row per task, keep children out of the sidebar, open them by mouse and keyboard, and return to the parent. Verify child views have no composer or dictation, including after completion/reload, while their permission responses remain usable. Preserve the parent's draft through quick navigation/reload and keep independent forks editable.
34. Interrupt a background delegation while it waits for permission, preserve Stopped after reload, and show a failed launch as one disabled task row rather than duplicate links.
35. Compare actual animation time against the document timeline before/after live reconnection, child navigation and reduced-motion changes. Both the main activity text and agent task status retain their shimmer phase; the activity has no spinner and stops when the turn finishes.
36. Generate a title from the first message while its task still awaits permission and has no assistant reply. Update the sidebar and open tab immediately, retain the title across reload, complete the task, and receive a title for another client's unopened conversation.
37. Open several conversations, revisit an older tab, open and close another, and return to the most recently active tab. Closing an inactive tab preserves selection, and reordering does not change activation history.

38. Submit an invalid write without creating a file or prompting for permission, render the precise field error, and recover in the same chat by reading a real file with a namespaced, capitalized tool name.

39. Fork at an assistant reply, show a source divider between inherited history and new messages, retain it through continuation and reload, resolve a renamed/archived source, and use the keyboard to return to the original reply in a long source conversation. A normal SSE refresh must preserve that position. An imported fork with an unavailable source keeps the source title and its own composer.

## Neutral tool errors — 2026-09-07

Tool results that contain errors now use the same neutral icon, rail and output colors as other completed activity. The visible Failed label is omitted for standalone tools and batch summaries/children. The original result and error metadata are preserved, and opening the disclosure still shows the returned message. Running-state styling is unchanged.

Two focused Electron scenarios passed in development and again in the signed packaged app against its bundled Haarness server. The checks compared computed colors for successful and unsuccessful calls, verified the absence of failure labels, retained missing-file messages, exercised standalone searches and mixed-result batches, and confirmed corrected tool retries still succeed. The standalone screenshot was visually inspected at `apps/desktop/test-results/neutral-tool-error.png`. Build/typecheck, whitespace and deep signature verification passed; the previously installed runtime was preserved byte for byte.

The installed desktop was restarted with this bundle; its new main process and renderer were confirmed running.

## Immediate terminal startup and closing — 2026-09-07

Expanding the terminal now starts and focuses a shell automatically, including from the toolbar, side-panel launcher and command palette. Reopening reuses existing terminals. Startup is shared across rapid panel remounts and scoped to its workspace connection. Closing a tab immediately deletes its PTY without a confirmation modal; closing the last tab hides the panel. The close path uses the server's returned terminal list so a concurrent lifecycle refresh cannot leave an empty panel behind.

All **35 desktop unit tests** passed. Three focused Electron scenarios passed in development and in the final signed macOS bundle against the actual Haarness server. They covered immediate focus, commands executed in the correct workspace, hide/reopen reuse, repeated toggles, output after reattachment, direct tab close with no dialog, process exit, survival of the remaining shell, last-tab hiding and fresh-shell creation on reopening. The existing workspace/file/review/terminal scenario also passed; its refresh check now waits for the refreshed inline comment before interacting with another control. Build/typecheck, whitespace and deep signature checks passed. The previously installed runtime was preserved byte for byte.

The packaged terminal screenshot was visually inspected at `apps/desktop/test-results/terminal-auto-start.png`.

The installed desktop was restarted with this bundle. Its new main process and renderer were confirmed running, and the installed signature passed verification.

## Agent stop controls and quiet states — 2026-09-07

Delegated task rows now show a smooth title shimmer while working and a separate square stop button for the active child run. Finished rows use compact icons instead of status text; approval and question icons retain accessible descriptions and hover labels. Waiting agents no longer claim to be executing their last tool. The stop action checks the child run identity, interrupts that child's tree, and preserves keyboard focus without opening the child conversation.

All **35 desktop unit tests** passed, including stale/terminal task identity guards. Four focused Electron scenarios passed in development and again in the signed packaged app against its bundled Haarness server. They verified live approval/completion/failure/stopped states, mouse and keyboard navigation, mouse and keyboard stops, parent/sibling isolation, permission cleanup, active controls after reload, terminal persistence, narrow-window targets, and continuous shimmer through reconnects/navigation/reduced-motion changes. The working, stopped and completed screenshots were visually inspected. Build/typecheck and whitespace checks passed; deep signature verification passed and the previously installed runtime was preserved byte for byte.

Screenshots: `apps/desktop/test-results/agent-controls-working.png`, `agent-controls-stopped.png`, and `agent-task-completed.png`.

The installed desktop was restarted with this bundle; its new main process and renderer were confirmed running, and the installed bundle passed deep signature verification.

## Fork origin divider — 2026-09-07

Forks persist `forkOrigin` with the source title, the actual last copied sequence and the original source item ID. The existing `parentId` identifies the source conversation. SQLite records the boundary in the same transaction as the copied history; newer clients do not have to infer it from timestamps or text. The Desktop divider uses the current source title when available and navigates to that original reply. Older forks retain their source link without inventing an exact boundary. Export/import preserves the title snapshot and boundary; sanitized exports redact the snapshot title.

All 39 packaged Electron scenarios passed (5.6 minutes). A separate screenshot check waited for the full restored history and confirmed the divider sits between the inherited reply and new messages. The release protocol (19), server (193), store (36), and TUI (297) suites passed: 545 tests. Store coverage includes exact boundaries, empty and whole forks, nested forks, persistence, older database migration, portable archives, and sanitation. The TypeScript and both Python SDKs passed their 103-operation suites, lifecycle tests, schema checks, and clean installed artifact checks. All 31 Desktop unit tests passed.

The added thread data exposed an oversized TUI picker variant; its thread payload now uses indirection without changing the wire format or behavior. The final TUI suite passed again (297 tests), and tmux exercised assistant-reply forking, continued input, and switching between the source and fork through the session picker. All-target Clippy passed with warnings denied. After rebuilding the runtime, both focused fork scenarios passed again on the final packaged app (4.8 seconds), including reading copied Markdown through macOS `pbpaste`. The isolated Copy test now awaits both its completion indicator and Electron's asynchronous clipboard read; assertions do not print unrelated clipboard contents on failure.

The final bundle, ZIP and mounted DMG passed deep signature and matching-payload checks. Delivered renderer SHA-256: `c6ccd3841567b7f591990060d3b08b3565cdf585dc284fdb027b0b985d23b59f`. Delivered runtime SHA-256: `7570ad2f1ce88665821d97b4e174ae12d3e0aef9cff35e207e540d5435513d7e`.

Desktop was reopened with the final bundle. Its main and renderer processes were running, and the restored project's authenticated runtime was healthy with the matching final executable fingerprint. No active user turn was stopped.

## Tool recovery and assistant-only message forks — 2026-09-07

Tool dispatch now repairs only unambiguous ASCII capitalization and the known `functions.` namespace. Exact registrations win, and resolution happens before availability and permission checks. Shared execution-schema validation supplies field paths and expected constraints before approval or execution, and revalidates hook rewrites. Unknown, deferred, and disabled tools receive distinct recovery instructions. Native execution schemas retain existing shell, todo, task-continuation, and workflow-file-context compatibility. Mixed batches retain valid results while recording each invalid or unavailable entry separately.

Desktop user-message footers retain Copy and omit Fork. Assistant-message forks include the selected reply and exclude later history. The TUI picker now offers the same assistant-only message boundaries. Live terminal testing also exposed and fixed ordinary forks being mistaken for read-only subagents: TUI presentation and draft restoration now use `isSubagent`, while preserving parent lineage. Forks stay editable and visible among conversations; delegated agents stay read-only.

The final packaged app passed **38/38 Electron scenarios in 3.7 minutes** using its bundled runtime. The message-action scenario verified user Copy from keyboard focus, absence of user Fork, assistant Copy/Fork, exact fork history, and the original conversation. The new tool-recovery scenario rendered the invalid field, verified no file or approval, and read a real file on the next message. The captured user footer was visually inspected at `apps/desktop/test-results/user-message-copy-only.png`.

**688 Rust tests passed**: 92 core, 71 provider, 193 server (including 86 API and seven tool-recovery regressions), 35 store, and 297 TUI tests. Desktop unit tests passed **31/31**. Release compilation, Clippy with warnings denied, TypeScript, formatting, and whitespace checks passed. The runtime cases cover corrected retries, nested field errors, exact-name collisions, availability/permission ceilings, hook rewrites, deferred loading, per-entry batch failures, legacy input compatibility, and real workflow attachment permissions/cancellation.

Real tmux walkthroughs passed using isolated projects. `scripts/test-tool-recovery-tmux.py` exercised rendered validation feedback, a successful corrected read, and a permission-gated shell command; its final run used the bundled runtime. `scripts/test-compaction-fork-tmux.sh` compacted a conversation, selected an assistant reply by keyboard, verified inclusive history, sent another message in the editable fork, and checked that the source was unchanged. Captures are `/tmp/haarness-tool-recovery-tmux.txt` and `/tmp/haarness-assistant-fork-tmux.txt`.

Deep signature verification, the DMG checksum, and bundle/ZIP/mounted-DMG payload comparisons passed for the application archive, runtime, Bun, voice helper, and launcher. Current SHA-256 values:

- `app.asar`: `d2daae20d04d6fda39a0775c80f8210c1e8e8d28a489cdf29930d066e57b12cd`
- Runtime: `c9424149dbaad771e5e3674de5ee2494dd28af837066338686f0c3d32edccd46`

The normal desktop was restarted after testing. Its main process and renderer were running, and the selected project's authenticated runtime was healthy with the exact bundled executable fingerprint. Existing user work was not stopped.

## Tab history, compact scroll control, and batch names — 2026-09-07

Closing a conversation tab now returns to the most recently active remaining tab. Activation history belongs to the current project and is independent of visual ordering. The jump-to-latest control is a circular down-arrow button with an accessible name and tooltip. The focused development Electron tests for tab history and streaming/scroll behavior passed, and the arrow screenshot was inspected (`apps/desktop/test-results/jump-latest.png`).

The runtime resolves the model's `functions.` namespace before dispatch and policy checks, preserving exact registered names. Batch planning uses canonical names, counts unknown/disabled/recursive entries as individual failures, and continues valid calls. Nested writes retain the normal permission path. The batch schema now explicitly asks for plain tool names.

The **86 API tests**, **five tool-catalog tests**, **six title regressions**, and **31 desktop unit tests** passed. The new runtime cases cover the reported `functions.list`/`functions.glob` batch, partial failures, recursion, exact-name collisions, disabled tools, and approval of direct and nested namespaced writes. A real tmux walkthrough issued `functions.batch` containing `functions.bash` and reached its normal permission prompt; the capture is `/tmp/haarness-batch-tmux.txt`. Release compilation and Clippy with warnings denied passed.

The batch unit test for ordered results and individual failures passed. Signature verification, the DMG checksum, and bundle/ZIP/mounted-DMG comparisons passed. Application archive SHA-256: `6e74f6ec8349b623d496293aebd3a5828294d5587c7b741b07f97e3954717d18`; runtime SHA-256: `c56ca9adaa1bceb3f99e2c378eca1d0eceb587cb73786229d0d0e1836f2c23f0`.

The final packaged app passed **37/37 Electron scenarios in 3.4 minutes**, including the new tab-history regression and the existing live scrolling, title, approval, daemon-upgrade, and subagent scenarios. Formatting and whitespace checks passed.

## Sidebar scrollbar — 2026-09-07

The sidebar scrollbar is transparent while idle, appears on scroll, then fades after 900 ms without further scrolling. A registered CSS property animates the existing native thumb without changing its width or the content's position; reduced-motion preferences disable the fade. Hovering the list alone does not reveal the scrollbar.

A real Electron walkthrough with 30 conversations passed in both development and the final packaged app. It verified idle/hover transparency, visibility after mouse-wheel scrolling, hiding after inactivity, stable layout/scroll position, and reduced motion. The idle, scrolling, and resting screenshots were inspected. This focused check uses `/tmp/haarness-scrollbar-smoke.mjs`, with screenshots under `/tmp/haarness-scrollbar-*.png`. Build/typecheck, formatting, and whitespace checks passed. The normal desktop was reopened after the packaged check.

Signature, DMG checksum, and bundle/ZIP/mounted-DMG comparisons passed. The final application archive SHA-256 is `06c6718c2ff0f5e1d1fc360f8233929467c86b57da78a097778418606a307823`; the runtime hash is unchanged from the first-message-title revision below.

## First-message titles — 2026-09-07

Title generation now starts after the first real user input passes prompt hooks, alongside the main provider request. Previously it started only after the turn finished and required an assistant reply. The title agent's configured model or small model is preferred, with the active session model as fallback. A per-thread job prevents duplicate requests; transient failures retry without changing the conversation's state. Atomic title updates preserve manual names and other thread metadata. Children and synthetic messages do not start title jobs.

The runtime publishes `thread.updated` as soon as the title is stored. Desktop tabs/sidebar and the TUI consume that existing event without waiting for task completion. **Six runtime title regressions**, **84 API tests**, the **four focused TUI tests**, and **31 desktop unit tests** passed. The title-specific development Electron scenario passed. A real tmux walkthrough (`scripts/test-title-tmux.py`) proved that the generated title was visible while the task was awaiting permission, with no assistant response yet; its capture is `/tmp/haarness-title-tmux.txt`. Release compilation, Clippy with warnings denied, TypeScript compilation, formatting, and whitespace checks passed.

The final packaged app passed **36/36 Electron scenarios in 2.1 minutes** with its bundled runtime. Deep signature verification, the DMG checksum, and the ZIP/mounted-DMG payload comparison passed. The application archive remains `ccc9c818638eac191073d8e47d54d5530e64daf4f00834911246e1e2ed815ff3`; the updated runtime is `dee8d8a2774ec4882a4bb3f650fbf3a1685a292e575d6389178dda13f0f6c2e9` (SHA-256).

These tests use controlled providers to prove timing, retries, fallback, metadata races, and live delivery. They do not measure title quality or latency from a paid model service.

## Continuous activity and read-only subagents — 2026-09-07

The packaged suite passed **35/35 in 3.5 minutes**, and desktop unit tests passed **31/31**. Build/typecheck, formatting and whitespace checks passed. Focused development checks covered the spinner-free slow-start sidebar, stream scrolling, continuous shimmer, read-only child views, parent drafts, dictation and final task states. The first draft-preservation check exposed the typing debounce losing unsaved text on a quick reload; navigation now flushes that draft immediately, and the regression passed in the full package run.

Child sessions omit the composer except for pending permission/question docks. The `isSubagent` flag controls this behavior, retaining composers for ordinary forks. Refresh snapshots and their newer events are applied together, and shimmer animations share the document timeline so recreated labels do not restart their sweep.

Deep signature verification and the ZIP/mounted-DMG payload comparison passed:

- `app.asar`: `ccc9c818638eac191073d8e47d54d5530e64daf4f00834911246e1e2ed815ff3`
- Runtime: `fb8bad5ad50a7024fb870fd913bbb05424ab580ce8e353983c39fc5acf481d96`

The normal desktop was reopened after the packaged suite. Screenshots: `apps/desktop/test-results/subagent-read-only.png` and `continuous-activity.png`. Runtime/TUI evidence below remains from the preceding runtime revision; these follow-up changes affect the desktop renderer only.

## Delegated task summaries and sidebar revision — 2026-09-07

The packaged suite passed **34/34 in 3.3 minutes**. Desktop unit tests passed **31/31**, including repeated call IDs, continued background tasks, completion/launch ordering, legacy classification, and parent activity. Focused older-runtime Electron checks covered the task rows, final states, streaming/scrolling, and Copy/Fork. The final release store suite passed **35/35**, including migration from the old schema, renamed forks, and standalone child archive import. Protocol tests passed **19/19**, the focused TUI thread tests **4/4**, and the server subagent lifecycle tests **3/3**. The live server schema and both generated SDKs match.

The UI, ZIP, and DMG share these verified payloads:

- `app.asar`: `bd891ea9269c2b5057ade2b357ed078862b48f7446a601d2ab36328e199a688e`
- Runtime: `fb8bad5ad50a7024fb870fd913bbb05424ab580ce8e353983c39fc5acf481d96`

Deep signature checks and the mounted DMG checksum/content comparison passed. The normal desktop was reopened after testing; its main process and renderer were verified. Existing work was not stopped. Screenshots of the actual approval and completed rows are in `apps/desktop/test-results/agent-task-approval.png` and `agent-task-completed.png`.

## Conversation presentation and icon system revision — 2026-09-07

The transcript was rebuilt around three shapes: a bubble for what the user said,
plain prose for the answer, and one hairline rail for everything the engine did.
User messages lost their repeated author label. Assistant replies keep a compact
timestamp beside Copy and Fork, revealed on hover or keyboard focus, in a footer
whose height is always reserved so nothing reflows. Tool calls became verb +
target steps — "Ran `npm test`", "Edited `src/app.tsx`" — sharing one continuous
rail, with state at the start of the row and "Done" shown only on hover.
Expanding an edit step shows the change as a diff rather than raw arguments.
Fenced code carries its language and a Copy control attached only after
streaming ends. Errors offer a local recovery action; a missing credential links
to Settings → Providers. The reading measure moved from 760px to 700px, about
ninety-five characters at the default conversation size.

Every functional icon now comes from `lucide-solid` through one catalogue in
`renderer/icons.tsx`, which maps role names to glyphs, uses a five-step size
scale and specifies stroke width in rendered pixels so optical weight is
constant. The hand-written SVG path table in `ui.tsx`, the inline caret in
`Select.tsx`, the `↗` symlink marker and the `✓`/`○` plan markers were replaced,
and roles that had shared one drawing — New folder against Reveal in file
manager, Archive against Restore, branch against fork — were separated. Pure
transcript rules moved into `renderer/transcript.ts` with unit coverage; the
bespoke sidebar menu was replaced by the shared `MenuButton`, whose items can
now carry a description used to say which checkout work will run in while
keeping their accessible name to the action. Around 3.5 KB of CSS belonging to a
screen that no longer exists was removed.

Every palette's `muted` text now clears 4.5:1 on `raised` as well as `page`,
`sidebar` and `surface`; six palettes needed a small adjustment and the themes
test was extended to enforce that fourth surface, which carries popovers, menus,
chips and the jump-to-latest pill. `--faint` remains deliberately below AA and
is now used only for decoration and hover-only affordances; content that had
been on it — step targets, timestamps, command descriptions, code language
labels, session times — moved to `--muted`.

Validation: all **32 development Electron scenarios** passed (1.6 minutes), as
did **24 desktop unit tests** (7 new for the transcript rules), TypeScript
compilation, the renderer build, and Prettier formatting of every changed file.
The **final packaged macOS app passed all 32 scenarios** (1.8 minutes) using
its bundled Rust runtime and Bun. Deep signature verification passed for both
the bundle and the read-only mounted DMG, the DMG checksum is valid, and the
Applications shortcut is present. The application archive, launcher, Rust
runtime, Bun and signed speech helper have identical hashes in the bundle and
in the mounted image. Application archive SHA-256:
`8c4662b73fe5c882bf34986926176b39af1a1c82abb2d45252012883d7df612d`. Signed
speech-helper SHA-256:
`4e4989de516dfed4fac8b494fe48fc83b88ed87ed43623cc87d0b31792a13788`.

A scripted walkthrough drove the real app through first launch without a
project, opening a project from the composer picker, streaming Markdown, a bash
permission and its approval, a three-tool turn with an inline edit diff, hover
message actions, long output, the Files and Review panes, a second project, the
anchored project picker, the Haarness menu, Settings, Light and Dark appearance
through the real control, and a 900px window. Screenshots of each state were
inspected in both appearances; the terminal dock is covered by the suite.

Limits: this revision changes only `apps/desktop` and the two desktop documents.
No Rust runtime, SDK or TUI code changed, so their prior validation stands
unchanged and was not rerun. Live microphone transcription remains unexercised
for the reasons recorded below. Contrast is verified against the token pairs the
themes test covers; it does not measure every composited surface in the running
app.

## Project picker, message presentation, and dictation revision — 2026-09-07

The shared home/composer project picker supports search, selection, New project, and general chats. General chats use a marked separate working location and their own Chats list, while global/project configuration retains the native scope model. Assistant prose omits the repeated author heading; timestamps and Copy/Fork icons sit below it. Reply forks include the selected answer. The scroll region no longer inherits the global focus outline that appeared as a full-width teal line. Real tool/stream activity drives a small shimmer label, with reduced-motion support.

The desktop has a native macOS dictation adapter using Swift, Apple Speech, and AVFoundation. The app and helper both include microphone/speech entitlements and usage descriptions. Dictation inserts editable text into the originating draft and never auto-sends. The helper shuts down when stopped, when its window reloads/closes, or when the app exits. Only the initiating window/run can control it, and stale events cannot modify another draft.

Validation: **32 development Electron scenarios** and **17 desktop unit tests** passed, along with TypeScript, renderer/native helper compilation, and formatting. The final packaged macOS app also passed all **32 scenarios** (2.0 minutes), including the compiled-helper probe and dictation subprocess/IPC tests. The native probe reports a supported/available on-device English recognizer on this Mac. Speech authorization is still undetermined, so actual microphone-to-text recognition has **not** been exercised: UI/IPC/lifecycle coverage uses a deterministic subprocess fixture. Live dictation requires a separate first-use macOS permission/hardware check. Windows/Linux native dictation is not implemented. No Rust runtime or TUI code changed in this revision; their prior validation remains listed separately.

The deep-signed bundle, ZIP, and mounted DMG match for the application archive, launcher, Rust runtime, Bun, and native speech helper. The DMG checksum and Applications shortcut passed. Application archive SHA-256: `a50abfdf553d816519678987f58a7cd47d69a3ddceb12bc7dda522760652de62`. Signed speech-helper SHA-256: `4e4989de516dfed4fac8b494fe48fc83b88ed87ed43623cc87d0b31792a13788`.

## Simplified controls revision — 2026-09-07

The desktop now has whole-row ordering, project-title expansion, one sidebar archive action, shared themed pickers, and a composer whose border indicates active work. The dedicated drag controls, movement menus and notices, separate Working/Stop strip, and send-key footer hint were removed. The composer uses a steady border while awaiting a permission/answer and honors reduced-motion preferences.

Validation passed **29 development Electron scenarios**, **14 desktop unit tests**, TypeScript compilation, renderer build, and formatting checks. A final focused picker test also verified that rapid consecutive keyboard selections start with a fresh type-ahead buffer. The **final packaged macOS app passed all 29 scenarios** using its bundled Rust runtime and Bun. The tests exercise real pointer/keyboard input, SSE, scoped settings forms, all palettes in Light/Dark/Auto, permissions, file/diff/terminal panes, Git worktrees, 10,005-file projects, and shutdown without stopping server work. Screenshots of the composer and dark/light pickers were inspected.

The final bundle, ZIP, and read-only mounted DMG have matching application archive, launcher, Rust, and Bun hashes. Deep application signature checks, DMG checksum, and the Applications shortcut check passed. The tested `app.asar` SHA-256 is `ef4ca46e3cacb12a013190e864dd3871e58f487c00f47f1f6183faf31cf142fa`. The desktop was relaunched normally after verification. This revision changes the renderer; the Rust/TUI and tmux evidence below belongs to the preceding runtime revision.

## Earlier project grouping, worktree naming, and practical scale revision

The desktop now groups a project's primary folder and registered Git worktrees in one sidebar section. A conversation remains bound to its exact working directory; new-chat drafts belong to the project, and existing-chat drafts use both directory and thread identity. New chat defaults to Project folder, with existing/new worktree choices in the composer. The canonical CLI/TUI command is `worktree` / `/worktree`; legacy API and command aliases remain compatible.

All **28 development Electron scenarios** and **28 packaged macOS scenarios** passed (1.5 minutes per full run). The packaged suite uses the bundled Rust runtime and Bun. The worktree scenario verifies real Git creation, independent approvals, correct file/review routing, persistent ordering, and separate drafts even when an imported conversation has the same thread ID in two checkouts. The large-project scenario edits the 10,005th file without a snapshot conflict and checks that directory listing includes every entry. Screenshots of the real app were inspected.

**14 desktop unit tests**, TypeScript compilation, the release build, and Clippy with warnings denied passed. The native package checks passed **716 tests**: CLI 85 (including integration tests), core 86, store 33, server 177, TUI 295, workspaces 16, extensions 10, profiles 11, and PTY 3. These are targeted package checks, not a full Rust-workspace run. Scale regressions include a complete 10,005-file snapshot capture/restore, an 80-tool extension, 21 simultaneous terminals, 4,100 portable profile resources, and a paged cache with 1,300 long conversation summaries. Separate real CLI runs verified Git-only automatic snapshot defaults and explicit enable/disable overrides.

The full `scripts/test-tmux.sh` walkthrough exited successfully, including project/worktree creation, switching, reset/removal, files/diffs, media, permissions, background agents, and process cleanup. `scripts/test-attach.sh` also passed. The first tmux run caught a background completion message hiding completed agents from navigation; the fix distinguishes synthetic continuations from a new user request, has a regression test, and passed the full rerun.

The desktop run caught a settings race: an automatic reconnect could restore the old trust policy while an explicit change was restarting a project. Per-directory transactions now commit runtime and saved settings together; background opens resolve the latest saved policy after preceding changes complete. The real Electron overlap test passed three repeated runs and both full suites. A separate auto-title race was fixed with an atomic conditional update so a manual rename cannot be overwritten by late generated text. Profile round trips now deduplicate identical migrated resources and restore plugins into their discoverable configuration directory.

The live Downloads task had finished before final delivery. A read-only user-style walkthrough confirmed that reopening it replaced the idle old daemon, retained all **32** existing transcript items without duplication, and kept Settings and New chat usable. No permission response or model request was sent. The application was then relaunched normally.

The tested bundle, ZIP, and mounted DMG contain identical renderer archive, launcher, Rust runtime, and Bun runtime hashes. Deep application signatures, DMG checksum, and Applications shortcut checks passed. Verification is local macOS; Windows/Linux behavior and public release signing/notarization remain unverified.

## Earlier project lifecycle and draft revision

The [project model](../DESKTOP-PROJECT-MODEL.md) separates saved navigation, independently refreshed project indexes, draft ownership, and task execution. The generic workspace connection screen and app-wide busy state are gone. Permissions and questions remain scoped to their task; waiting conversations have a sidebar indicator and an explicit waiting status instead of an animated “Working” indicator. Project opening is coalesced in the main process and coordinated across CLI processes with a workspace-specific launch lease.

All twenty-six Electron scenarios passed in development (52.1 seconds) and the packaged macOS app (57.2 seconds). Eight desktop unit tests, TypeScript compilation, the release runtime build, all 77 CLI release tests, and CLI Clippy with warnings denied passed. The full `scripts/test-tmux.sh` gate also passed against the same release runtime. It exercises real terminal input and rendering, process groups, suspend/resume, focus and mouse hover, slash completion, workflows, plugins, media, permissions, agent handoff, cleanup, and frozen-daemon recovery. `scripts/test-attach.sh` passed its authenticated network, execution, fork, media, and TUI detach/reattach checks.

A separate user-style Electron walkthrough exercised first launch, a model selection before the first message, editable starter prompts, file browsing beside a draft, the composer project picker, independent drafts, Enter-to-send, Light appearance, and a 900 px window. Screenshots were inspected. This work caught and corrected first-message attachment reuse, draft/navigation races, IPC serialization of cached project metadata, and goal creation leaving its management pane. A full-suite failure caused by an image fixture changing the first reviewed file was corrected by isolating that fixture outside the Git project.

The real Downloads incident was also checked in the packaged UI using the existing installation. Its original daemon instance and active task were preserved, the pending permission was visible, and Settings remained usable. No permission response or new model request was sent during that check. The application is relaunched normally after package verification.

The ZIP and mounted DMG match the tested application archive, launcher, Rust runtime, and Bun runtime. Both application signature checks, the DMG checksum, and the Applications shortcut check passed. These local checks do not establish Windows/Linux behavior or public release signing.

## Earlier scope alignment, direct menus, and streaming scroll revision

That revision passed all twenty-three development Electron scenarios (46.7 seconds) and all twenty-three packaged-app scenarios (48.9 seconds). Eight desktop unit tests, TypeScript compilation, the release runtime build, and CLI Clippy with warnings denied passed. Release Rust tests passed for the CLI (77), profiles (9), and server (177): **263 tests**. The server count includes unit and integration suites. These are targeted package checks, not a full Rust-workspace result.

The full desktop run initially exposed two regressions: unrelated pane scrolling dismissed an open action menu, and virtual transcript height contraction could incorrectly stop automatic following. Both were corrected before the successful development and packaged runs. Streaming scroll tests use real wheel and keyboard input against the actual Electron renderer and daemon. The final scope checks also cover inherited project extension enablement, ensuring the switch reflects the runtime default without copying global entries into project configuration.

The [configuration scope model](../CONFIGURATION-SCOPES.md) defines shared client behavior. Native clients now share XDG configuration/data/cache/state resolution, project overrides, scoped resource management, and separate provider credential storage. JSON-only settings forms and the redundant active-turn guidance sentence were removed from the desktop.

On this existing installation, the new data path aliases the original storage directory. All six pre-migration conversation databases retained their original filesystem identity, and sixteen database/sidecar paths resolved to the same files. The credential file retained its identity and mode 0600. No live database copy or conversation deletion was performed.

The ZIP and mounted DMG match the tested application archive, launcher, Rust runtime and Bun runtime. Deep application signature checks passed for both the bundle and mounted image, as did the DMG checksum and Applications shortcut checks. The updated packaged app was relaunched after verification.

## Earlier independent project expansion revision

Projects now have separate disclosure and activation controls, with their own conversation lists, filtering and ordering. Expansion is stored by project ID and remains independent of active workspace selection. Expanded inactive projects cache thread/turn metadata and maintain an SSE subscription; collapsing them releases that subscription without stopping their work. Concurrent activation requests for the same project share one load, so clicking conversations while it opens preserves open tabs.

The focused new scenario passed, followed by all eighteen development Electron scenarios (34.6 seconds) and all eighteen packaged-app scenarios (35.9 seconds). Eight desktop unit tests, TypeScript compilation, formatting checks, deep app signature verification, and DMG checksum verification passed. The ZIP and mounted DMG match the tested application archive, launcher, Rust runtime, and Bun runtime; the mounted app signature and Applications shortcut were also verified. The full suite initially exposed a concurrent activation race through the existing conversation-tab restoration test; the shared-load correction passed the unchanged regression. This revision changes desktop renderer behavior; the runtime and SDK checks reported below belong to earlier revisions.

## Live errors, ordering, and panel launcher revision

Terminal failures now use the canonical durable-item publisher before the completion event. Project selection updates its existing entry instead of moving it to the front. Projects, sessions, conversation tabs, side-panel tabs, and terminal tabs share scoped ID ordering, persistence, drag targets, keyboard controls, and accessible feedback. Rows remain keyed by identity across live updates. The side-panel launcher provides Files, Review, and Terminal, and returns when the last panel tab closes.

All seventeen Electron scenarios passed in development mode (29.9 seconds) and in the packaged macOS app (36.9 seconds). Eight desktop unit tests, 86 core tests, two server event tests, the release runtime build, desktop TypeScript compilation, formatting checks, and deep application signature verification passed. The new provider test exercises an Anthropic-compatible provider with no credential and a loopback-only endpoint; it makes no external model call.

The preceding simplified navigation and slash-command revision passed all twelve scenarios in development (49.5 seconds) and in the packaged app (38.7 seconds). Five desktop unit tests also passed, covering durable event replay, IPC/navigation validation, and a minimum 4.5:1 contrast ratio for primary, muted, and body text on the main surfaces of all sixteen palette/appearance combinations. Screenshots of the real app are produced under `apps/desktop/test-results`; failures retain Playwright traces.

## Workspace-opening revision

The extra project-trust wizard has been removed. Opening a new folder enables project configuration, MCP servers, hooks, and extensions by default. Explicit saved workspace settings remain respected. The desktop connector applies configuration-policy changes through the authenticated daemon replacement path, which preserves history and rejects replacement during active turns. The advanced setting remains available in Settings → Servers & workspaces.

All thirteen Electron scenarios passed in development mode (26.5 seconds) and in the packaged app (28.5 seconds). Five desktop unit tests, two targeted Rust desktop tests, the release runtime build, desktop TypeScript compilation, Rust formatting, and deep package signature verification also passed. The broader runtime and SDK results below belong to the preceding revision; they were not rerun for this workspace-opening change.

## Earlier runtime and SDK checks

The preceding revision removed the retired Teams feature from the desktop, runtime, model tools, CLI/TUI commands, HTTP routes, protocol schemas, generated SDKs, and documentation. Regular background subagents and `/btw` side questions remain supported. New databases no longer create the retired coordination tables; the reserved migration version preserves existing session history and leaves historical metadata untouched.

The TypeScript SDK, synchronous Python SDK, and asynchronous Python SDK each passed all **103** remaining operations with the rebuilt release runtime. Their lifecycle checks passed, including owned-server startup/shutdown, SSE streaming, cancellation, and timeouts. Four TypeScript transport tests, six Python tests, and Python type checking passed. Packed npm and Python wheel artifacts were installed in clean consumers and passed the same 103-operation scenarios (including Python 3.10) and TypeScript declaration checks. Live schema comparison and generation-drift checks passed.

Targeted release tests passed across the CLI (77), core (86), store (32), server (176), and TUI (294): **665 tests** in total. These include absence of the retired routes/tools, the ordinary background-task lifecycle, isolated side-question HTTP rendering, and reopening an existing database without losing sessions. The TUI group was rerun after correcting stale expectations in its side-question test.

The release runtime build, desktop TypeScript compilation, Rust formatting, architecture boundaries, and commercial boundary check passed. The packaged app passed deep signature verification.

The earlier full-workspace debug gate and this revision's debug compilation stalled on this Mac. They are not recorded as successful full-workspace runs. CI retains the full-workspace gate.

## Reproduce

```sh
cargo test --release -p haarness-core -p haarness-store -p haarness-server -p haarness-tui -p haarness-cli -p haarness-workspaces -p haarness-extensions -p haarness-profiles -p haarness-pty
cargo build --release -p haarness-cli
HAARNESS_BIN="$PWD/target/release/haarness" ./scripts/test-desktop.sh
HAARNESS_BIN="$PWD/target/release/haarness" ./scripts/test-tmux.sh
HAARNESS_BIN="$PWD/target/release/haarness" python3 scripts/test-title-tmux.py
HAARNESS_BIN="$PWD/target/release/haarness" ./scripts/test-attach.sh
npm --prefix apps/desktop run package

HAARNESS_BIN="$PWD/apps/desktop/release/mac-arm64/Haarness.app/Contents/Resources/bin/haarness" \
HAARNESS_DESKTOP_EXECUTABLE="$PWD/apps/desktop/release/mac-arm64/Haarness.app/Contents/MacOS/Haarness" \
npm --prefix apps/desktop run test:e2e
```

Use the bundled executable for packaged testing: daemon ownership includes the executable fingerprint. A newly signed macOS executable may be inspected by the OS before its first instruction runs; the test fixture allows extra startup time while keeping normal interaction/SSE assertions bounded.

## Evidence limits

The local ZIP and DMG contain the tested macOS application. The DMG was created with native `hdiutil`, following the working packaging path established during the earlier build. Its checksum was verified, it was mounted read-only, its application/runtime hashes were compared to the tested bundle, and its application signature was verified. The image includes the usual Applications-folder shortcut.

This is a locally signed macOS build. Public release signing, notarization, a hosted update feed, and publishing were not performed. Windows and Linux packaging are configured in CI but were not executed on this Mac. Provider OAuth, paid model responses, and encrypted remote-server connections were not exercised with real accounts. The test provider exercises the real daemon, persistence, transports, tools, and Electron UI while making the model response deterministic.


## Bundled ripgrep — 2026-09-07

The runtime now embeds ripgrep 15.2.0. Agent shell tools and integrated terminals
receive the verified executable through their child process PATH. Login-shell
startup cannot hide it by resetting PATH. The desktop bundles the upstream
notices alongside the Rust executable and Bun.

All six macOS/Linux/Windows arm64/x86_64 compressed payloads and executable hashes
passed offline verification. Four extraction tests passed: actual binary execution,
corruption/deletion repair, concurrent first use, missing execute permission,
symlink rejection, and login-shell paths containing spaces and shell metacharacters.
New-crate all-target Clippy passed with warnings denied; architecture boundaries,
formatting and desktop build/typecheck passed. Both existing shell-tool regression
tests passed in release mode, covering workdir confinement, exit metadata and
timeout schema. The local debug build stalled and was stopped before those
release checks.

`scripts/test-ripgrep.py` passed on both the release CLI and the final signed
packaged backend. It copies only the executable into a separate directory,
uses a fresh cache and restricted PATH with unavailable network proxies, and
exercises authenticated Echo-provider turns, permission replies, Unicode/spaced
paths, ignore rules, glob flags, pipelines, exit codes 0/1/2, repair while the
server runs, real PTY execution, and CLI `run --attach` output.

The new Electron scenario passed in development and the packaged app. It submits
an actual shell tool through chat, checks rendered search output and the resolved
bundled path, then types a search in xterm and verifies the resulting file. The
packaged terminal open/close regression scenario passed too. Screenshot:
`apps/desktop/test-results/bundled-ripgrep-chat.png`.

Deep signature verification passed. The installed desktop was restarted and its
new main/renderer processes were confirmed. All four running workspace servers
were authenticated and matched the installed executable fingerprint. Linux and
Windows execution is covered by the added CI gate; local end-to-end execution
was on macOS arm64.

All three existing PTY regression tests also passed in release mode: buffer
trimming, detach/replay/resize, and opening more than twenty terminal sessions.


## Project required for desktop conversations — 2026-09-07

Both project pickers offer existing projects and New project. The general-chat
option, IPC mode, automatic send fallback, separate Chats section and general
starter cards were removed. Fresh startup disables composition until a project
is selected; New chat opens the folder chooser. Cancelling does not create a
working directory or a conversation.

Legacy general chats become ordinary projects in their existing folders at
startup. Connection IDs, conversation history, drafts and trust settings remain
intact. The migration is idempotent and is persisted before the window opens.

All 35 unit tests passed. Five focused Electron scenarios passed in development
and again in the signed packaged app against the bundled server: fresh startup
and cancelled selection, rejection of the removed IPC mode, project selection
and draft persistence, slow project activation, worktree navigation, and legacy
history/draft restoration across reloads. The empty-state screenshot was visually
inspected. Build/typecheck, formatting and signature verification passed.

The installed backend was preserved byte for byte. The app was restarted, its
main and renderer processes were confirmed, and the actual saved legacy location
was verified to retain its identity, working directory and trust setting after
migration. The previous bundle and desktop state were retained as backups.


## Sidebar worktree view — 2026-09-07

Replaced the separate Worktrees page with a per-project sidebar toggle. Enabled Git projects show
Local and worktree groups with branch names, independently collapsible sessions,
per-checkout new-chat buttons, creation, and checkout actions. Plain folders keep
the ordinary session list. View and expansion preferences survive restarts.

New-chat drafts are independent per checkout in this view. Existing conversations,
permissions, file/review panels, and terminals retain their concrete location.
Flat view preserves its previous project draft behavior and session ordering.
Creating from an inactive project's sidebar targets that project even while
another project is selected; completing an earlier project activation does not
steal the new-worktree navigation.

Reset/removal serialize against checkout opens, check activity, and close an idle
server before acquiring the backend workspace lease. Pending approvals, active
tasks, workflows, and running terminals block those operations. Dirty checkout
removal is refused. Reset offers explicit tracked-only and tracked-plus-untracked
discard choices; ignored files are retained. Missing registration cleanup and
normal removal update the sidebar without removing conversation data.

All 35 unit tests passed. Seven focused scenarios passed against the final signed
macOS arm64 bundle and its real Haarness server: independent project expansion,
row ordering/title interaction, slow project activation, existing worktree
conversation/draft/permission/file routing (including imported thread IDs), the
new grouped view and reset/removal flow, discovery/missing-checkout cleanup and
inactive-project creation, and project-picker draft persistence. The grouped
flow also checks per-checkout terminal cwd and process reuse, draft restoration,
keyboard collapse/expansion, view toggling, dirty/active-task protection, and
removal from the actual workspace registry. The sidebar screenshot was visually
inspected at test-results/worktree-sidebar.png.

Build/typecheck and signature verification passed. The bundled backend remained
byte-for-byte identical to the previously installed runtime. The verified app was
installed and restarted; main and renderer processes were confirmed. The prior
bundle and desktop settings were retained as backups. Nothing was published.

## Unavailable worktree visibility — 2026-09-08

Unavailable worktrees are hidden by default in the sidebar and working-location
picker. Their cached rows and connection errors also stay out of the flat session
list. The project menu can temporarily reveal unavailable worktrees in worktree
view for inspection or stale-registration removal; reload returns to the default
hidden state. Revealed entries use the clearer “Folder not found” label.

Two focused scenarios passed against the signed packaged app and its real server:
the existing grouped worktree, draft, terminal, reset and removal flow; and
discovery, unavailable-folder hiding, explicit reveal/hide, reload, flat view,
picker filtering, original-folder restoration with retained conversation history,
stale-registration cleanup, and inactive-project creation. The restoration
fixture moves the original folder aside and back, preserving its Git identity.
Build/typecheck, whitespace and signature checks passed. The bundled backend was
preserved byte for byte.

## Universal search — 2026-09-08

Search now combines application and contextual commands, all 15 settings
sections and individual controls, configured shortcuts, projects/worktrees,
active and archived conversations, current-workspace files, and available
provider/model/agent/command/skill/tool/MCP/plugin settings. Modules register
contextual actions with a shared index. Exact titles, words, aliases, normalized
shortcut notation, and compact fuzzy title matches are ranked together.

Settings targets scroll into view and receive focus. Escape closes only Search
and restores the prior focus, including inside Settings. Slash commands preserve
the existing draft and wait for the user to send. Project and worktree results
activate their exact locations; matching conversations are no longer limited to
eight. Collapsed project indexes are refreshed while Search is open, and
unavailable worktrees remain excluded. File-search responses are discarded when
the query, workspace, or search lifetime changes.

All 38 unit tests passed. Five focused scenarios passed against the signed
packaged macOS app and bundled Haarness server: settings/control/shortcut/file
and slash-command navigation; more than eight matching conversations, archived
history and exact worktree routing; unavailable worktree hiding and restoration;
terminal startup/close behavior; and fork visibility, archive, project switching
and reload. The legacy fork test was adjusted to respect persisted expansion
rather than clicking an already expanded project closed after reload. The
application-closing lifecycle test now sits last in the shared-window suite.

Build/typecheck and signature verification passed; the existing backend binary
was preserved byte for byte. Universal search and settings-result screenshots
were visually inspected and retained in the installation staging evidence folder.

## Auto approvals and composer — 2026-09-08

Desktop now has persistent Auto/Review controls in the composer and universal
search. Modes are scoped to a conversation or a workspace default; child agents
inherit their ancestors' mode, while ordinary forks, other projects and worktrees
retain their own settings. “Allow once + Auto” from a child view affects that
child and its descendants without enabling the parent or siblings. It enables
only after a successful reply and an unchanged view/mode. TUI supports Ctrl+Y,
the permission-mode command, and the same one-time approval action with a
launch-local mode. Auto never stores Always rules; explicit denies, agent
ceilings and manual questions remain enforced. See AUTO-APPROVALS.md for the
scope and reconnect behavior.

The composer uses the requested rounded input, left-side attachment and approval
controls, and right-side agent/model/effort, dictation and send/stop controls. Both
normal and narrow-window screenshots were inspected.

The combined agent test exposed an existing server event-ordering race: concurrent
publishers could allocate cursors in one order and deliver approval events in
another. Cursor allocation, replay insertion and live delivery are now serialized,
with an atomic replay-to-live subscription handoff. A regression test reproduces
the original failure and passes after the fix with 8,000 events from eight threads.

Final validation passed against the updated release runtime:

- 43 desktop unit tests, 300 TUI tests, 93 core tests and 2 server event integration
  tests (438 total).
- Eight real-server tmux scenarios: existing pending requests stay manual when
  enabling TUI Auto; successful Once + Auto; subsequent automatic replies;
  background sessions and child agents; questions and explicit denies; Review
  prompts again for the exact same command; real daemon replacement/reconnect;
  and TUI exit/relaunch resets the mode.
- Six scenarios against the signed packaged macOS arm64 app and its bundled
  server: Auto/Review, reload persistence, agent inheritance and plan ceilings;
  child-only Auto with a simultaneous manual sibling and Search-based Review;
  independent real Git worktree/project scopes, collapsed-project activity and
  narrow composer layout; live SSE chat, guidance, approvals, questions and stop;
  dictation editing/lifecycle; and universal search navigation.
- Typecheck/build, formatting, whitespace and strict bundle signature checks.

The deterministic test provider drives real runtime tools and filesystem effects;
these scenarios do not call a paid model. The bundled backend matches the tested
release binary byte for byte (SHA-256
`0b6bf73fd5f933178238704c1592b53fb1238a6f38f50abc1b4ea9ed9e4af620`).
Logs, screenshots and the pre-fix event-ordering failure are retained in
`/var/folders/x5/7p86qtnd17gf_c1vvzbc0ygm0000gn/T/haarness-desktop-auto-final-wj9am2dm/evidence`.

The verified bundle was installed and Haarness restarted. Main and renderer
processes were confirmed, and both the installed application archive and backend
match the tested bundle by SHA-256. The previous app and desktop settings were
retained as backups. Nothing was published.

## Concrete new-chat models — 2026-09-08

Removed the unresolved “Default model” entry. The composer now resolves its
provider/model before sending, using explicit selection, agent preferences,
workspace configuration, valid recents/history, then a connected provider's
preferred model with tool support favored. Ordinary conversation history can
seed a choice; child-agent history cannot. Manual selections persist as recents,
unavailable automatic candidates are skipped, and existing conversations keep
their explicit model. With no available provider, the composer offers connection
setup. Effort controls follow the selected model before the first message.

The server exposes optional configuration preferences in `/v1/info`; TypeScript
and Python SDK types were regenerated from its real OpenAPI document. Desktop
reads these preferences with the catalog and sends the exact displayed pair on
conversation creation.

All 48 desktop tests, 300 TUI tests, four TypeScript SDK tests and six Python SDK
tests passed. SDK generation, Python type checking, desktop typecheck/build,
formatting and whitespace checks passed. Six scenarios passed against the signed
macOS arm64 bundle and bundled server: concrete model selection, Auto approvals,
live SSE chat, slow project startup, themed pickers and dictation. The new model
scenario uses a local OpenAI-compatible HTTP fixture with real model discovery;
it verifies configuration and agent overrides, repeated manual selections,
project switching, reload persistence, selected effort, stored thread model, and
the model/effort actually received by the provider. No paid model was invoked.

The composer screenshot was visually inspected. Evidence is retained in
`/var/folders/x5/7p86qtnd17gf_c1vvzbc0ygm0000gn/T/haarness-desktop-model-bnlxfrb0/evidence`.

The verified bundle was installed and restarted. Its application archive and
backend match the tested artifact by SHA-256, and main/renderer processes were
confirmed. The prior app and settings were retained; nothing was published.


## 2026-09-08: Schedule controls and grouped conversations

Schedule settings now use daily, weekday, weekly, interval and one-time controls, with custom cron available explicitly. Agent, model and supported reasoning effort come from the connected catalogs; editing preserves existing settings and can clear a saved effort. Settings show the selected checkout's definitions without history, logs or operation-output panels.

Scheduled conversations appear below ordinary chats, grouped by durable schedule ID and checkout. Groups paginate in batches of ten, retain expansion during live updates, and reopen around the selected conversation after reload. New metadata survives renaming, database reload, archive export/import and schema migration; sanitized archives redact schedule names. Independent forks remain ordinary conversations, and old schedule titles retain compatibility.

The real native test exposed two one-shot lifecycle faults: unloading the launchd job killed its runner before recording the result, and then before releasing the overlap lock. Completion now persists the disabled state and result, releases the lock, removes the launch file, then unloads. Completed historical one-shots can be edited while disabled; re-enabling requires a future time. A rescheduled trigger is retained if an earlier run completes later.

Validation of this revision:

- Rust: 157 tests passed across core (93), protocol (19), store (37) and scheduler (8). All eight scheduler tests passed again after the final lock-release adjustment.
- Desktop: 52 tests passed, including picker round-trips, date/interval validation, legacy recognition, renamed schedules, same-name schedules, checkout isolation and fork classification.
- SDKs: four TypeScript and six Python transport tests passed; Python type checking passed. The generated SDKs match the real rebuilt server's schema.
- The signed packaged app passed five Electron/server end-to-end scenarios: concrete model selection; Auto approvals and reload; schedule creation/editing/native execution/grouping; sidebar pagination/live updates/reload; and worktree navigation/drafts/terminals/lifecycle.
- The schedule scenario exercised every frequency, retained custom cron on edit, rejected invalid cron and past dates, selected a pinned agent model and reasoning effort, cleared that effort, registered and fired a real launchd job, verified its actual provider request and saved conversation, confirmed automatic disable, ran it again manually, renamed it, reloaded, forked, archived a run, and removed the definition while preserving history.
- Desktop build, Rust formatting, diff whitespace checks, live schema comparison, SDK generation check and strict deep bundle signature verification passed. Native execution was tested on macOS; Linux and Windows scheduler rendering remain covered by unit tests.

Evidence is retained in the directory referenced by `/tmp/haarness-schedule-stage-path`, under `evidence/`. The package is local and unpublished.

Installed the exact tested app into `apps/desktop/release/mac-arm64/Haarness.app` and restarted it successfully. Both the runtime and application archive hashes match the tested bundle; main and renderer processes were observed. The previous app and desktop settings were backed up in the staging directory.


## Dynamic model catalogs and shared names — 2026-09-08

Successful provider discovery now determines catalog membership, including empty
lists. Static metadata fills missing friendly names, limits and pricing without
reintroducing removed models. Refreshes are cached and coalesced; failures and
incomplete pagination preserve the last successful list. Providers without a
model-list API retain their configured/curated fallback. See MODELS.md for
provider coverage, precedence and refresh behavior.

Desktop and TUI pickers show connected providers only. Desktop resolves a saved
unavailable choice to a usable model and sends the exact displayed pair. With no
usable model, provider setup opens and the chat draft is preserved. Names are
shared by the Rust and Desktop surfaces, including schedules and agent rows.
Current OpenAI/Claude metadata was checked against official documentation and
live account catalogs. Claude adaptive thinking and native structured output
follow the supported model capabilities.

Validation completed:

- 397 Rust tests passed: protocol 20, providers/cache 76, TUI 301. The TUI command
  completion test now expects provider setup when its fixture has no providers.
- 54 Desktop unit tests passed, alongside typecheck and production build.
- Three development Electron/server scenarios passed: provider failure/replay,
  concrete model selection, and dynamic catalog/provider-setup behavior.
- Live testing used the authorized existing Robowork credentials through process
  environment variables. No credentials were added to source or Desktop settings.
  A private Haarness server discovered 89 conversational OpenAI models and 11
  Anthropic models. Seven actual model turns passed: GPT-6 Astra, GPT-5.6 Sol,
  Terra and Luna, Claude Fable 5.1, Opus 5 and Sonnet 5. Astra and Fable 5.1 also
  completed real read-tool round trips; Sol exercised reasoning `none`, and
  Fable 5.1 exercised `high`. Only synthetic prompts and a temporary file were
  submitted. The live test recorded 11 successful checks.
- Real tmux testing passed against the final bundled runtime: friendly names,
  connected-only choices, refresh without restarting TUI, selecting a newly
  discovered model, removing stale choices and empty-catalog provider setup.
- All 104 server operations passed through each SDK path: TypeScript, Python
  sync and Python async. Four TypeScript and six Python transport tests passed.
  The generated clients and schema match the final bundled server.

Evidence is retained under the staging directory referenced by
`/tmp/haarness-model-stage-path`, in `evidence/`. The opt-in live test is
`scripts/test-models-live.py`; the terminal test is `scripts/test-models-tui.py`.

Seven scenarios passed together against the signed macOS arm64 bundle and its
actual bundled server: provider failure/replay, concrete model selection, live
catalog refresh/provider setup, Auto approvals, schedule controls/native
execution, delegated-agent rendering and independent agent stop controls.
The dynamic case verifies provider setup with a retained draft, discovery through
stored credentials, live metadata precedence, keyboard selection after catalog
changes, a saved conversation's connected fallback and actual streamed response,
last-success retention after HTTP 503, and authoritative empty lists. Screenshots
of the connected and empty model states were inspected.

Packaged test setup now attaches with the runtime inside the bundle, avoiding
intentional server replacement caused by the identity of an external executable.
The narrow agent-controls scenario explicitly closes a Files panel left open by
earlier scenarios. Both setup failures and the final successful combined run are
retained in the evidence directory. Neither required a production change.

Installed the exact tested bundle into
`apps/desktop/release/mac-arm64/Haarness.app` and restarted Haarness. Independent
readback confirmed the installed runtime and application archive SHA-256 hashes
match the tested artifacts, and main/renderer processes are running. Strict deep
signature verification passed during installation. The previous app and desktop
settings were retained in staging. The build and SDKs remain unpublished.


## Modal outside-click dismissal — 2026-09-08

Schedule create/edit forms, shared input/confirmation dialogs, Settings and
universal search now share backdrop dismissal. A complete primary-button click
outside the panel follows its existing Cancel/Close path. Inside clicks, dialog
padding, dropdown selections and drags across the panel boundary leave it open.
Native dialogs close before unmounting so focus returns to the opening control.
Dismissing a child dialog leaves its parent Settings panel open.

The new Electron/server scenario verifies Settings, unsaved schedule creation
and editing, schedule-removal cancellation, a provider form, and universal
search. It checks both drag directions, nested dropdowns, focus restoration,
Escape, normal Save/Confirm actions, and actual server state after cancellation.
It passed in development and in the signed macOS arm64 app. Four packaged
scenarios passed together: modal dismissal, full schedule controls/native
execution, themed pickers, and universal search navigation. The picker fixture
now supplies a sufficiently large connected catalog to exercise its search box.

Typecheck, production build, whitespace checks and deep signature verification
passed. Evidence is retained in the staging directory referenced by
`/tmp/haarness-modal-stage-path`, under `evidence/`. The bundled Rust runtime was
reused from the previously verified model build without source changes.

Installed and restarted the exact tested bundle. Independent readback confirmed
both runtime and application archive hashes match the tested artifacts, with
main and renderer processes running. The prior app and desktop settings remain
backed up in staging. Nothing was published.


## MCP setup form — 2026-09-08

Replaced the shared MCP prompt with a dedicated local/remote form, ordered argument/value rows, environment and header key/value editors, masked values, OAuth settings, project/global scope, and an enabled toggle. Local and remote drafts survive transport switches, while only the selected transport is persisted. The native desktop asset route now delegates MCP writes to the existing management service through JSON on stdin; environment and header values do not become process arguments or unredacted management output.

Validation completed:

- Desktop typecheck, production build, and all 57 desktop unit tests passed. The new conversion tests cover argument boundaries, empty environment values, duplicate keys, header case sensitivity, malformed targets, and OAuth/default behavior.
- Electron development test passed against the rebuilt release runtime. A real Python stdio server received reordered arguments (including a path containing spaces), a synthetic token and an empty environment value, then answered a tool call. A real HTTP MCP server received Authorization and X-Tenant headers during initialization, listing and tool execution.
- The final signed macOS bundle passed three end-to-end scenarios together: the new MCP form, existing native agent/command/skill forms, and backdrop dismissal across modal surfaces. The MCP scenario also covers project/global persistence, enabled/disabled OAuth configuration, duplicate protection without overwrites, draft retention after failures, keyboard focus during row editing, transport switches, narrow layout, Escape/backdrop dismissal, Settings refresh, and renderer reload.
- Inspected screenshots of local, remote and 540-pixel layouts. No horizontal overflow in the narrow form. OAuth settings were verified through native persisted configuration; the form test does not open a real external OAuth sign-in page.
- Installed and restarted Haarness. The installed executable and app archive hashes match the packaged test artifact; deep signature verification passed. The previous app and desktop settings are retained in the staging directory. Nothing was published.

Evidence: `/private/var/folders/x5/7p86qtnd17gf_c1vvzbc0ygm0000gn/T/haarness-desktop-mcp-t92x25cd/evidence`.

Runtime SHA-256: `db4b4baae861a5b4b75fca09d4c5a58fa31cbabbb87421a9bc12186ccb56215b`.

Application SHA-256: `7c9607034edb634ce84dbe0acc9230bd4a21640460090652f4ff5d3ec05e044c`.

Native management regression: `cargo test --release -p haarness-cli --bin haarness management::tests -- --nocapture` passed all three tests. The MCP round trip now uses the desktop asset route and verifies project-scoped headers/OAuth, global arguments/environment, redacted results, and duplicate rejection without changing the saved file.

OAuth protocol regression: `cargo test --release -p haarness-mcp oauth_pkce_lifecycle_reaches_a_protected_mcp_tool -- --nocapture` passed. The real HTTP fixture exercises PKCE authorization through a protected MCP tool call. This complements the form-to-config OAuth checks without using an external account.


## Resource forms and MCP regression — 2026-09-08

Commands, agents and skills now have dedicated editors with larger instruction areas, descriptive scope choices, connected-model pickers, command agent/argument controls, skill notes, and structured agent tool overrides. Editing resolves the selected row's scope before loading, so a global definition cannot accidentally edit the project definition with the same name. Unexposed frontmatter and permissions remain intact.

Custom providers separate API format, authentication and model discovery; keys use the native credential channel. Workflow inputs use typed rows initialized from metadata. Plugin, marketplace and extension installers show source-specific fields and native browse controls. A shared editor retains drafts after failed saves, reports errors inline, restores focus on dismissal, and clears stale validation when a picker is changed.

Validation completed:

- Production build/typecheck and all 59 desktop unit tests passed. Two new workflow conversion tests cover declared/inferred types, structured defaults, exact text values, duplicates and invalid inputs.
- Ten Electron/server scenarios passed together in development (37.0 seconds), followed by the focused provider regression after the final picker fix. The final signed macOS arm64 bundle passed all ten together (49.6 seconds).
- Real execution covers a saved command with its chosen custom agent/model and substituted arguments, typed workflow results and execution defaults, a local HTTP provider's authenticated model discovery and streamed response, installed plugin/extension flows, marketplace registration, project/global isolation, unknown metadata and permission preservation, validation failures, modal dismissal and narrow layouts.
- MCP was retested in both runs with actual stdio and HTTP servers: ordered arguments, environment values, HTTP headers, tool discovery/calls, OAuth configuration, duplicate protection, scope persistence, renderer reload and dismissal. Native release tests passed all three management regressions and the PKCE OAuth lifecycle through a protected MCP tool call.
- The combined test initially exposed shared fixture-directory churn: watching the extension's folder also watched the MCP capture JSON, triggering repeated reload candidates. Extension fixtures now live in their own source directories. The final run keeps extensions enabled while adding and using MCP servers. A packaged assertion also caught a stale picker validation message; the final build fixes it and retains the regression check.
- External services use local protocol fixtures and synthetic credentials. These checks do not claim every provider protocol or a remote npm/Git package download was exercised. Screenshots were inspected at normal and narrow widths.

Installed and restarted the exact tested bundle. Independent readback confirmed matching runtime/application hashes, strict deep signature verification, and running main/renderer processes. Previous app and desktop settings are backed up in staging. Nothing was published.

Evidence: `/private/var/folders/x5/7p86qtnd17gf_c1vvzbc0ygm0000gn/T/haarness-desktop-forms-xchd1cgv/evidence` (unit/native logs, development and packaged reports, screenshots, and installed-artifact readback).

Runtime SHA-256: `db4b4baae861a5b4b75fca09d4c5a58fa31cbabbb87421a9bc12186ccb56215b`.

Application SHA-256: `244aa7f1af1270577b4f429208be063f97f843dfaddfdbc8a1027578148ce77d`.


## Select outside-click dismissal — 2026-09-08

Reproduced the intermittent reopening in the shared select: clicking its wrapping label first dismissed the menu on pointerdown, then the browser forwarded a click to the button and reopened it. Native dialogs can also focus themselves between those events. Label clicks now toggle once without that intermediate focus change. Outside pointer and focus listeners run during capture so other controls cannot prevent dismissal by stopping bubbling. Cancelled opening microtasks cannot move focus after a menu closes.

The regression scenario repeatedly opens/closes four provider-form selects through their labels, switches directly between controls, intercepts outside events, commits an option, uses keyboard dismissal, and checks draft preservation. It also exercises MCP forms, Settings and the composer. The unchanged component failed the label assertion; the fix passed all three focused Electron/server scenarios, including existing picker and modal regressions. The production build/typecheck and all 59 desktop unit tests passed.

The final signed macOS arm64 bundle passed all 12 Electron/server scenarios together (1.1 minutes): the new dismissal regression, existing picker and modal checks, settings, agents/commands/skills, scope isolation, MCP, custom provider discovery/execution, workflows and installation forms. MCP used actual local stdio and HTTP servers and verified arguments, environment, headers, configuration and tool execution.

Installed and restarted the exact tested bundle. Independent readback confirmed matching runtime/application hashes, strict deep signature verification and running main/renderer processes. The previous app and desktop settings remain backed up in staging. Nothing was published.

Evidence: `/private/var/folders/x5/7p86qtnd17gf_c1vvzbc0ygm0000gn/T/haarness-select-dismiss-_ggqxt9c/evidence`.

Runtime SHA-256: `db4b4baae861a5b4b75fca09d4c5a58fa31cbabbb87421a9bc12186ccb56215b`.

Application SHA-256: `7c4a43c895481bd0a0ef02ab5d71e6dff712935f9c0a0144715088ed00fc85ca`.


## Chats first in universal search — 2026-09-08

Search now places Chats before Archived chats and all action/settings/project/file groups. An empty query shows the eight most recent active chats first. Typed queries keep relevance ordering within each group and continue to search every conversation. The first chat receives the initial keyboard selection.

The production build/typecheck, three existing search unit tests and both packaged Electron/server universal-search scenarios passed (17.3 seconds). These cover settings destinations, keyboard activation, shortcuts, commands, files, archived history and worktree navigation. Inspected the packaged screenshot showing Chats above actions with the first chat selected. No new test suite was added for this ordering change.

Installed and restarted the exact tested bundle; runtime/application hashes and deep signature verification passed, with main and renderer processes running. Previous app/settings are retained in staging. Nothing was published.

Evidence: `/private/var/folders/x5/7p86qtnd17gf_c1vvzbc0ygm0000gn/T/haarness-search-order-eqpkqtmr/evidence`.

Application SHA-256: `5ef1b55c6ec1bc7a95795900b1bc823b9bcd5714c47a51406f9bdf5f198f21a6`.


## Hooks settings and project memory — 2026-09-11

The production renderer build/typecheck and runtime build passed. All 82 CLI unit tests and 82 desktop unit tests passed. Three real Electron/server scenarios passed together (23.1 seconds): hook creation and execution, stale draft retention, refresh/toggle, disabled and deleted hooks remaining inactive; memory create/read/delete, conflicting edits, persisted settings and live tool availability; and existing select/form dismissal behavior. Tests wait for runtime configuration reload events before sending hook proof messages.

Native regression coverage includes hook source scopes, plugin read-only presentation, unsupported event rejection, sibling/metadata preservation, memory index limits and disablement, project isolation, stale revisions, legacy data-directory aliases and refusal of linked memory files. Inspected Hooks and Memory screenshots. Refreshed the running development app and verified both editors load against its existing installation. Remote host configuration editing remains unavailable, matching the existing native management bridge.

Evidence: `/tmp/haarness-hooks-memory-verification-20260911`. No package was published or installed by this change.


## Project home and conversation loading — 2026-09-12

The production desktop build, TypeScript check and debug runtime build passed. All 82 desktop unit tests passed. All 17 selected Electron/server scenarios passed together (2.8 minutes).

Coverage includes first launch with no composer or project tools, cancelled folder selection and native-menu dispatch, explicit project opening and a real echo turn, history/draft restoration, saved projects without automatic first-project selection, missing-folder retry and recovery, legacy chat migration, server-scoped project selection and failure recovery, attachments retained across Home, tab ordering/reopening, files/review/terminal, reconnecting event streams, and multiple projects on a remote host.

The conversation-loading test delays an actual HTTP history response, verifies the centered Haarness mark and reduced-motion behavior, records zero project-home/new-chat flashes, and confirms a late response cannot overwrite a newer chat selection. The only supplied OS interactions are deterministic folder-picker results and native-menu dispatch; servers and conversation execution are real, using the test provider.

Inspected the project-home and centered-loading screenshots. Evidence, logs and the Playwright report: `/tmp/haarness-project-home-verification-20260912`. No packaged application was installed or published.


## Settings presentation cleanup — 2026-09-12

Removed raw server and MCP capability disclosures, architecture details, runtime log controls and the internal OS identifier. About and the remaining refresh/extension descriptions use plain language. Runtime capability handling is unchanged.

Production build/typecheck and all 82 desktop unit tests passed. Both Electron/server settings and universal-search scenarios passed together (35.3 seconds), including provider/catalog navigation, plugin and extension operations, settings search, shortcuts and files. Updated a stale test expectation for the already available Hooks page. Visually inspected Servers and About screenshots.

Evidence: `/tmp/haarness-settings-cleanup-20260912`. No packaged application was installed or published.


## Real server and attached TUI scenarios — 2026-09-12

Rebuilt the debug runtime. Nine Electron/server scenarios passed together (1.7 minutes), followed by the standalone Desktop-detach scenario (9.3 seconds including setup). Both the CLI/TUI attach gate and the multi-host tmux connection gate passed.

New regression coverage starts a server explicitly with `serve --daemon`, rejects wrong credentials through Desktop without storing them, adds the server in Settings, and exchanges messages live between Desktop and `tui --attach` in one conversation. An attached tool writes only in the server project. TUI reattachment, Desktop reload, and replacement of the server at the same address preserve history and allow both clients to continue chatting. Closing Desktop leaves pending server work alive and able to complete.

Broader checks cover server/project isolation and draft restoration, unavailable hosts, migration, Home and loading states, remote file context and terminal working directory, approvals/questions/interruption, fork isolation, attachment upload, and TUI host/project navigation. The attach script accepts `HAARNESS_EVIDENCE_DIR` to retain terminal captures and conversation records.

Evidence: `/tmp/haarness-real-connections-20260912`. Actual servers, Electron and tmux were used with the local Echo test provider; external model services, TLS deployment and a second physical machine were not tested. Test daemons and tmux sessions were cleaned up. No application installation/publication or connection product-code changes were needed.


## Optional server passwords and connection form — 2026-09-12

The Desktop connection form now requires only the server URL. Name is optional, and the optional username/password fields are collapsed initially. Password visibility, progress, preserved input after failures and plain inline error messages are included. Blank usernames default to `haarness`. Saved credentials use OS encryption, while existing encrypted bearer connections migrate without re-entry.

The server, CLI, TUI, TypeScript SDK and Python SDK support anonymous or HTTP Basic password authentication. Manually started servers accept anonymous connections by default; automatically managed local daemons retain private internal credentials. Authentication follows REST, event streams, remote project routing and terminal WebSockets. Explicit daemon reuse refuses conflicting authentication settings, and client launchers preserve manually configured server policies. Host/Origin validation remains active. A related TUI terminal fix preserves the remote project route during the WebSocket handshake.

Validation passed:

- Production Desktop build/typecheck, debug runtime build, all 82 Desktop unit tests, six TypeScript SDK tests and seven Python SDK tests; Python mypy passed.
- Four server authentication tests, seven TUI API tests (including a real WebSocket handshake checking project routing and Basic authentication), and CLI parsing across serve/attach/tui/run.
- All seven Electron/server scenarios together: server/project scoping, legacy credentials, project Home, delayed conversation loading, shared Desktop/TUI chat and file execution, anonymous and password connections with blank names, wrong-user correction without losing form input, encrypted credential restoration after app restart, remote-project chat and terminal execution in the correct folder.
- Real CLI/tmux flows for anonymous connections and Unicode passwords, rejected wrong passwords, server-side tools, server restart, reattachment and history recovery. CLI help conceals credential environment values. Legacy token attach also passed.
- The multi-host tmux connection/security suite passed, including optional password entry, remote projects and draft restoration after restart. The test now waits for the draft to appear before quitting.
- An additional server using the normal configured OpenAI provider returned `Connection ready.` through a real model request.

Inspected collapsed and expanded form screenshots. Restarted the development Desktop app. Evidence and reports: `/tmp/haarness-optional-auth-verification-20260912`. The repeatable integration suites use the local Echo provider; the additional OpenAI request used a live service. TLS deployment and a second physical machine were not tested. No packaged application was installed or published.


## Remote folder explorer — 2026-09-12

Replaced the remote project dialog with a compact folder explorer and removed the known-projects/catalog section and Browse button. The editable path bar requests autocomplete from the server and supports arrows, Tab, Enter and Escape. Back, Up, Home, hidden-folder visibility, bounded pages, loading/error states and keyboard folder navigation are included. Navigation resets the list scroll position; opening failures preserve the entered path.

The authenticated host directory endpoint supports case-insensitive prefix filtering, hidden folders, page limits/cursors and server-side home expansion. Responses retain only one bounded page plus a look-ahead entry in memory while scanning. The picker does not depend on the project catalog.

Runtime and production Desktop build/typecheck passed, along with all 82 Desktop unit tests. Four real Electron/server scenarios passed together (40 seconds): the new explorer, anonymous/password connection flows, and existing multi-project chat/files/terminal/draft recovery. The final navigation/loading polish passed the focused explorer scenario again (8.4 seconds). The explorer test covers 208 visible folders, hidden entries, Unicode paths, stale autocomplete, keyboard completion, parent/back/home navigation, missing paths, no catalog dependency, light/dark/narrow layouts and opening a folder followed by a real Echo turn. Additional live API checks passed for symlinks, unreadable paths, root/home navigation and cursor ordering.

Inspected screenshots and restarted the development Desktop. Updated the user playground server on the same address while preserving its conversations. Evidence: `/tmp/haarness-folder-explorer-verification-20260912`. No package was installed or published; a second physical machine and TLS deployment were not exercised.


## Remote project opening latency and startup failure — 2026-09-12

Removed global workspace/Git scans from individual lookups and the two CLI startup metadata checks. Project opening reuses the verified server information and loads history and model choices concurrently. Friendly failures now distinguish timeouts, credentials, access, missing folders and busy servers instead of always blaming the path.

An instrumented Electron open of `/Users/alex/Desktop` against the normal configured server and full user registry dropped from 10,846 ms (warm baseline) to 952 ms (final run with the project daemon stopped first). These are individual runs, not statistical benchmarks. The final run exchanged a real provider message successfully. A new regression reproduced the startup deadline failure with 60 unrelated saved workspace records, then passed after the fix with zero Git probes in those folders. It also covers retained input, timeout/retry and daemon restart recovery.

All 12 distinct Electron scenarios passed across the retained runs, including anonymous/password servers, real tmux `tui --attach`, shared chat and file execution, folder autocomplete/pagination, chat loading, project isolation, and slow-start navigation/draft restoration. Two older assertions were updated for the existing folder combobox and the requested loading view; their passing reruns are retained alongside the initial failures. Production build/typecheck, 83 Desktop unit tests, 17 workspace tests, and the CLI database-migration and snapshot-policy regressions passed.

The normal development Desktop was restarted, and the playground server remains available on `http://127.0.0.1:64706` without a password. Evidence: `/tmp/haarness-remote-open-verification-20260912`. No package was published or installed; a second physical machine and TLS deployment were not exercised.


## Chat-only Desktop composer — 2026-09-12

Removed the Chat/Shell selector, mode shortcuts and search action, shell draft/history handling, direct shell submission and Desktop's `runShell` bridge operation. The composer always uses the normal chat submission path. Existing chat history retains its storage key, and legacy shell drafts/history are ignored. This change concerns the Desktop composer; agent tools, integrated terminals and other clients retain their independent execution APIs.

Production build/typecheck and all 83 Desktop unit tests passed. Five real Electron/server scenarios passed together (25.1 seconds): chat-only entry points and history/draft restoration, first-message attachments, streaming/guidance/permissions/questions/interruption, agent colors, and file/agent prompt context. The new scenario verifies removed shortcuts do not switch modes, search offers no switch action, the command bridge rejects direct shell execution, legacy shell history stays out of chat recall, and command-shaped text is sent through a model turn. Inspected the composer screenshot and restarted the normal development Desktop with its existing profile.

Evidence: `/tmp/haarness-chat-only-verification-20260912`. No package was installed or published.


## Header and focus treatment cleanup — 2026-09-12

Removed the conversation-context icon from the chat header. Context remains available through Universal search and the side-panel launcher. Replaced the global green focus underline, inset shadow and tint with a neutral keyboard-only indicator; pointer clicks and menu focus restoration no longer draw that decoration. Reset default fieldset borders across forms and render management Name fields as single-line text inputs.

Production build/typecheck passed. Four real Electron/server scenarios passed together (22.3 seconds): panel navigation with pointer/keyboard focus assertions, dropdown dismissal and retained drafts, context accounting/search/reload, and remote profile creation. The profile check verifies no outer border, the single-line name, dropdown focus without a shadow or outline, dark/light screenshots and a successful save. Visually inspected the profile and keyboard screenshots. The normal development Desktop was restarted with its existing profile.

Evidence: `/tmp/haarness-control-cleanup-verification-20260912`. No package was installed or published.

## 2026-09-12: tool sandbox removal and standard permissions

Removed the tool policy store, macOS Seatbelt wrapper, Linux bubblewrap wrapper, hostname proxy, web-fetch restrictions and post-approval sandbox write restrictions. Removed the Sandbox settings page, `/sandbox` command, management resource and reserved sandbox approval category. Previously saved `sandbox.json` files are ignored, including malformed files. Electron renderer isolation and the portable workflow JavaScript context are separate application internals; neither imposes OS restrictions on agent tools.

Ordinary tools run by default, external locations require permission, and reads matching `*.env` / `*.env.*` ask while `*.env.example` is allowed. Haarness now covers named files such as `service.env` as well as `.env`. User allow/ask/deny rules, session/working-folder grants and agent permission boundaries remain active. Haarness-specific agent-switch/media confirmations and loop protection remain.

Desktop approval headings now describe the action, such as “Access a folder outside this project,” without displaying internal permission category identifiers.

Passed:

- `cargo test -p haarness-core -p haarness-server -p haarness-tui -p haarness-cli`: 690 tests, zero failures.
- Desktop production build/typecheck and 83 unit tests.
- Three real Electron workflows: Auto/Review and reload; reviewed sharing with the removed management API rejected; remote-server ordinary tool execution, networking, external write approval/denial, environment-file prompts, configured deny, and obsolete policy migration. The remote permissions workflow passed again after the final heading change.
- `python3 scripts/test-permissions-attach.py`: nine real anonymous-server + `tui --attach` scenarios, including Allow once, Deny, session Always command-prefix reuse, default shell execution on the remote host, environment reads and retired policy migration. An initial test incorrectly expected an unknown-command error; the final test verifies the existing ordinary-chat fallback for removed commands.
- `python3 scripts/test-tui-connections-security.py`: host networking and permitted external writes, private sharing/expiry/revocation, two servers, project gateways and drafts surviving a full TUI restart.
- SDK E2E: all 105 operations passed through TypeScript, synchronous Python and asynchronous Python clients.

These workflows used actual servers and tools with a deterministic provider and isolated test data. Evidence (logs, screenshots and terminal captures): `/tmp/haarness-permissions-verification-20260912`. Desktop was restarted with the final build; the idle user playground server was restarted on the same address, `http://127.0.0.1:64706`, without changing its conversation history.

## 2026-09-12: open windows survive local UI rebuilds

The reported Settings failure came from an open window requesting a content-hashed module that a later build had removed from `out/renderer`. Unpackaged Desktop launches now copy the renderer and preload into a private session directory before loading a window. Every window and reload in that process uses that same set of files, and quitting removes the copy. Relaunch loads the latest build. Packaged releases and the live development server keep their existing loading paths. Trusted IPC checks use the session entry point.

Unexpected view errors now show a readable recovery message; raw exception details and local paths go to the diagnostic console instead of the user interface.

Verification: Desktop build/typecheck and 84 unit tests passed. Real Electron regression coverage replaces the original hashed Settings module while a conversation draft is open, then verifies Settings, draft preservation on reload, cleanup on exit and the new module on relaunch. A separate forced-load failure verifies the recovery text hides paths and exceptions. The existing real-workspace chat/file/review/terminal scenario also passed. Test builds and data were isolated; the user's build files were not mutated by the regression tests.

Evidence: `/tmp/haarness-ui-rebuild-verification-20260912`. Desktop was restarted with the fix; servers were left running.

## 2026-09-12: broader file syntax highlighting

The editor now detects languages with the bundled CodeMirror language catalog instead of seven hard-coded language families. Parsers load on demand from local app assets. Common configuration names and variants are recognized, including Dockerfile/Containerfile, Jenkinsfile, shell startup files, JSONC, uppercase extensions and extensionless scripts with a shebang. Unknown files remain editable plain text.

Language changes reconfigure the existing editor without replacing the document or undo history. Deferred imports cannot update an editor that was closed or changed to another language. Core editor code remains in its shared chunk, while language parsers stay in separate chunks instead of all loading with the editor.

Passed: build/typecheck, 86 unit tests, and real Electron checks of 13 file formats in both dark and light themes (YAML, TOML, shell, Dockerfile, Go, Java, C++, SQL, Swift, Kotlin, TypeScript, Python and plain text). The tests inspect rendered token colors, switch files, edit/save a shell script and verify existing unsaved-edit/conflict handling. Screenshots and logs are retained at `/tmp/haarness-language-verification-20260912`. Desktop was restarted with the new build; servers were left running.

## 2026-09-12: editable definitions and a simpler header

Removed the Files and Review titlebar shortcuts. Both remain available through
the side-panel tab launcher. Existing Electron tests now use that launcher.

Agents, commands and skills use working Edit / Save changes forms locally and
on remote servers. Built-in and plugin definitions create same-name overrides;
Reset to default removes the override. The server preserves agent permissions,
skill metadata and supporting files. Plugin originals stay unchanged. Built-in
agent overrides do not duplicate picker entries or change their hidden status.
Namespaced plugin commands and plugin skill shortcuts can be overridden too.
Background model refreshes no longer replace an open Settings editor.

Verification passed: 88 CLI tests, 96 core tests, 86 Desktop unit tests,
build/typecheck, and five Electron workflows. The local and password-protected
remote workflows each exercise five definitions, save/reopen/reload/edit again/
reset, original plugin bytes, skill support files and authoritative runtime
command/skill catalogs. The remaining workflows cover the tab launcher, real
file editing/review/terminal use, and structured editor metadata/scope retention.
Evidence: `/tmp/haarness-assets-final-e2e`, `/tmp/haarness-assets-regression`,
and `/tmp/haarness-assets-*-tests.log`. Desktop and the idle loopback playground
server were restarted; the server remains at `http://127.0.0.1:64706`.
