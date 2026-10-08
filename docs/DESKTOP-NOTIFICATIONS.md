# Notifications that open the conversation

Enable **Notifications** in Desktop settings. While Haarness is in the background, a notification identifies the conversation and project when a task finishes, fails, needs approval, or asks a question. Click it to bring Haarness forward and open that conversation, including switching to its saved server and exact remote project or worktree. The chat's unsent draft stays with it.

Notifications keep working for projects opened during the current app run even when their sidebar group is collapsed. They survive renderer reloads; on macOS, where closing the last window leaves Haarness running, clicking a notification creates a window again. Quitting the application stops notification monitoring. OS notification permissions and Focus settings still control whether banners appear.

By default, Haarness suppresses system notifications while any of its windows has focus. Turn completion notifications can also be set to Always or Never in Settings → General. Interrupted turns do not produce a success alert. Delegated agent completions stay quiet; their approval requests and questions open the parent conversation containing the controls. Failed tasks have a distinct **Task failed** title.

A **Task finished** banner waits half a second (`COMPLETION_GRACE_MS` in `src/main/notifications.ts`, the value Upstream measured) before it goes out. The daemon starts the next queued prompt, or the reply to steering a stop left unread, as a new root turn moments after the previous one finishes, so a banner sent at once would call work finished while it is still going. If the same conversation starts another turn, stops or fails inside the grace, the held banner is dropped; a newer completion replaces an older one. A queue of prompts therefore produces one **Task finished**, after its last turn. Failures, approvals and questions are never held, and a stop never produces a banner.

Clicks preserve existing conversation history and archive status. Removing a saved project or server dismisses its notifications and stops its monitor. An unavailable destination reports an error; it does not create a replacement conversation. Existing unsaved-file navigation protections still apply.

## Verification

From the repository root, with dependencies already installed:

```sh
npm --prefix apps/desktop run typecheck
npm --prefix apps/desktop test
(cd apps/desktop && npx electron-vite build)
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- e2e/notifications.spec.ts
```

The Electron suite creates three disposable Git repositories, including an authenticated remote project. Real server events drive real Electron `Notification` instances and the production click/IPC/navigation path. It exercises project switches, separate drafts, collapsed groups, foreground/muted suppression, interruption, a queue of prompts announcing one completion, actionable approvals/questions, delegated agents, reload, minimization, failed/archived conversations, rapid clicks, event replay, and macOS window recreation. `tests/notifications.test.ts` drives the completion grace with fake timers.

Only folder selection and OS banner delivery are substituted. This verifies the app's notification callback end to end; it does not assert that macOS displayed or delivered a physical banner. That last boundary requires a packaged-app check with notifications allowed: background Haarness, finish a task, click its system banner, and confirm the correct conversation opens.

Verified on 2026-09-13: 120 Desktop unit tests, all 9 notification Electron scenarios, the existing conversations/files/review/terminal Electron regression, TypeScript checking, and the renderer/main production build passed. The final Electron run completed all 10 tests in 48.6 seconds.

## Notification preferences

**Settings → General** includes a master **Task notifications** switch plus:

- **Turn completion notifications:** Only when unfocused (default), Always, or Never. Applies to completed and failed root tasks.
- **Permission notifications:** approval alerts while the app is in the background.
- **Question notifications:** input-needed alerts while the app is in the background.

The master switch suppresses all three. Notification clicks continue to open the originating project and conversation. Preference changes are rechecked after asynchronous conversation-title lookup. These controls are searchable inside Settings and through global search.
