# Schedules

> **User guides:** [Schedules](https://haarness.ai/docs/cli/schedules) on your computer, and
> [Cloud schedules](https://haarness.ai/docs/cloud/schedules) that run while it is off. This page is
> the engineering view of local schedules.

A local schedule is a prompt that runs in a project folder on a timetable. Haarness keeps no
scheduler process of its own: each enabled schedule is registered with the operating system's
scheduler, which starts the `haarness` executable at the right time. That run starts or reuses the
folder's server and creates an ordinary chat, so the result appears in the TUI, Desktop and the API
like any other chat.

A schedule a chat of Desktop's **This computer** makes runs in that chat's own folder, and only that
chat sees and changes it; see [A chat's own folder](PERMISSIONS-AND-EXECUTION.md#a-chats-own-folder).

## Managing schedules

```sh
haarness schedule add --name morning-review --cron '0 9 * * 1-5' --model openai/gpt-5.6 \
  --prompt 'Review this workspace and summarize urgent work'
haarness schedule add --natural 'every day at 8am to summarize the repo'
haarness schedule add --every 2h --prompt 'Check the release status'   # 30m, 2h, 1d; at least 1 minute
haarness schedule add --at 2026-12-01T09:00:00+01:00 --prompt 'Draft the release notes'
haarness schedule list                      # get NAME, update NAME, enable/disable NAME, remove NAME
haarness schedule run morning-review        # now, waiting for the answer
haarness schedule history morning-review    # recent runs, newest first
haarness schedule logs morning-review       # where its log is
```

`--cron` takes five numeric fields (minute hour day month weekday, Sunday is 0). `--natural` reads
one phrase with the timing and the prompt: "every day at 8am to …", "every Monday at 9am to …",
"in 10 minutes to …" or "on 2026-12-01 to …". `--model` is `provider/model`; a provider Haarness does
not know is refused when the schedule is created, and a model the provider does not list is only a
warning, since some providers serve more than they list. `--directory` picks the project (default: the
current folder), `--agent` and `--variant` the agent and reasoning, `--timezone` the zone for cron
times on Linux, and `--disabled` saves without registering. Other entry points use the same code:
`/schedules` in the TUI, **Settings → Capabilities → Schedules** in Desktop, `POST /v1/manage/schedules`
(the server-authored form), and the model's `schedule` tool (create, list, update, delete, enable,
disable, run, history). The model's `cloud_schedule` tool is the Cloud counterpart.

## Where things live

| What | Path |
| --- | --- |
| Definitions | `<config>/scheduler/jobs/<id>.json` (owner-only) |
| Run history | `<state>/scheduler/runs/<id>.jsonl` |
| Overlap locks | `<state>/scheduler/locks/<id>.json` |
| Run logs | `<state>/logs/scheduler/<id>.jsonl` |
| macOS | `~/Library/LaunchAgents/dev.haarness.schedule.<id>.plist` (launchd) |
| Linux | `haarness-schedule-<id>.service` and `.timer` in `~/.config/systemd/user/` (`systemd --user`) |
| Windows | Task Scheduler task `Haarness\Schedule\<id>`, its XML in `<config>/scheduler/tasks/` |

`<config>` and `<state>` are the operating system's application folders for Haarness
(`~/Library/Application Support/dev.haarness.haarness` on macOS; `~/.config/haarness` and
`~/.local/share/haarness` on Linux); `$HAARNESS_HOME/config` and `$HAARNESS_HOME/state` when
`HAARNESS_HOME` is set, and `config/` and `data/` under `HAARNESS_DATA_DIR` when that is set, so an
isolated install never shares the person's schedules. The native entry carries the storage variables
it was created with (`HAARNESS_HOME`, `HAARNESS_DATA_DIR`, the `XDG_*` folders). `HAARNESS_HOME` and
`HAARNESS_DATA_DIR` do not move the launchd and systemd folders, which the operating system reads.
Setting `HAARNESS_SCHEDULE_SKIP_INSTALL=1` writes definitions without registering them with the
operating system (the run a definition starts carries the variable too, so a run a test starts
natively registers nothing either), and puts the launchd and systemd ones in `<config>/scheduler/launchd/` and
`<config>/scheduler/systemd/` instead, where neither loads them at the next login. This is how the
end-to-end scripts and the Desktop specs test schedules (`scripts/e2e_env.py`,
`apps/desktop/playwright.config.ts` and `apps/desktop/e2e/fixtures/isolated-env.ts` set it); a spec
that runs one natively registers it from there itself and takes it back. On a disposable machine,
`HAARNESS_NATIVE_SCHEDULE_E2E=1` has `desktop.spec.ts` let Desktop register it instead, and checks
that it did.

## What a run does

Every native entry runs the hidden command
`haarness --workspace <folder> [--disable-project-config] schedule trigger <id> --source <backend>`
(`crates/haarness-cli/src/main.rs`, `execute_schedule`):

1. A disabled schedule, or one whose previous run still holds its lock, records a **skipped** run
   and exits. A lock whose process has gone is cleared (see [Schedules](REFERENCE.md#schedules) for
   the exact rule).
2. It starts or reuses the folder's server over the authenticated loopback API.
3. It creates a chat titled after the schedule, marked with the schedule's id and name, and starts
   one turn with the stored prompt, model, agent and variant.
4. That turn and everything it starts (subagents, workflows, background completions) approve ordinary
   permission prompts on their own and cannot use the question tool, since nobody is there to answer.
   Requests only a person can answer, such as a site in the user's Chrome, fail at once instead of
   waiting ([Chrome approvals](PERMISSIONS-AND-EXECUTION.md#chrome-approvals)).
5. A shell command the agent ran in the background is waited for (it stops at the `timeout` the agent
   gave or `execution.shell_timeout_ms`, even when the agent gave none), and so is the turn its end
   starts; a foreground command never moves there by itself in a scheduled run.
6. When the turn ends it records **success** or **failed** with the chat and turn ids, the answer or
   the error (with what the agent said when its commands ended). A one-shot schedule then disables
   itself.

Credentials are read from the normal `haarness auth` store or the environment when the run starts;
none are written into the native entry.

## Code and tests

- `crates/haarness-scheduler`: triggers and natural phrases, the job store, run history and locks,
  and the launchd, systemd and Task Scheduler backends. `cargo test -p haarness-scheduler`.
- `crates/haarness-cli/src/main.rs`: the `schedule` commands and the trigger path above.
- `crates/haarness-server/src/tools.rs`: the model's `schedule` tool. `cargo test -p haarness-server schedule`.
- `crates/haarness-cli/src/management/ui.rs`: the `schedules` management form used by the TUI and Desktop.
