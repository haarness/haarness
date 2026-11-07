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
   the exact rule). launchd, systemd and Task Scheduler start one run of a schedule at a time, so a
   time that comes while a run is still going (or a catch-up after the computer slept) is skipped
   without a record.
2. It starts or reuses the folder's server over the authenticated loopback API.
3. It creates a chat titled after the schedule, marked with the schedule's id and name, and starts
   one turn with the stored prompt, model, agent and variant.
4. That turn and everything it starts (subagents, workflows, background completions) approve ordinary
   permission prompts on their own and cannot use the question tool, since nobody is there to answer.
   Requests only a person can answer, such as a site in the user's Chrome, fail at once instead of
   waiting ([Chrome approvals](PERMISSIONS-AND-EXECUTION.md#chrome-approvals)).
5. The run then waits for its own work: the subagents, workflows and shell commands its turn started
   in the background, the turns their results start, and what those turns start in turn, down to
   the workflows its subagents start (a foreground subagent's too, even once it has ended) and the
   turns their results start in a subagent's chat. Work a person starts in the chat meanwhile is
   theirs: the run neither waits for it nor stops it. A message a person sends while one of the
   run's turns works joins that turn, and once the agent has read it the rest of the turn answers
   them: what the agent says from then on is not in the run's answer, and what it starts from then
   on (subagents, workflows, commands, and the results they bring) is theirs. The run still waits
   for the turn itself. How long it waits:
   - Subagents, workflows and the turns their results start have 30 minutes, counted from the end of
     the first turn, or from the end of the run's last command with a time limit if that came later.
     A workflow deleted meanwhile (`/workflow delete`, or the agent's own `delete`) has ended, and so
     has one whose result the agent already took with a `wait`, however much else the chat is owed.
   - A command with a time limit of its own (the `timeout` the agent gave, or
     `execution.shell_timeout_ms`) is waited for until it ends, however long that limit is, and so
     is the turn its end starts. Short of the 12 hours below, nothing is stopped while one runs.
   - A command with no time limit (`execution.shell_timeout_ms` set to 0) does not hold the run.
     When everything else has ended, it is stopped, the run still succeeds, and its record's `note`
     names the command.
   - Nothing holds a run for more than 12 hours from its start, its first turn and commands with a
     time limit included, so a run that keeps starting commands cannot keep its lock forever.

   When the 30 minutes or the 12 hours run out, whatever of the run's own work is still going is
   stopped and the run is recorded as **failed**, naming it. Nothing the run stops tells the chat or
   a chat under it, so no turn of the run starts in either after the run. A person's turn still may:
   the result of their own work, the answer to a message they wrote into a turn the run stopped
   before its agent had read it (the agent is told the run ended, and replies to the message), or a
   message they queued behind such a turn, which is sent once it stops, just as after a turn that
   ended. The times are wall-clock time, so the time a computer spends asleep counts. Questions
   asked in the run's own turns and subagents are declined, since nobody is there to answer; a
   person's question in the chat, or in a chat under it, is left for them. A server older than the
   `haarness` running the schedule cannot stop a run's work: the run is recorded as **failed** when
   it has work to stop, and that work goes on.
6. It records **success** or **failed** with the chat and turn ids and the agent's answer: what it
   said in the run's own turns. The run fails, with the reason as its error, when its first or last
   turn failed or was stopped, or when a subagent it ran in the background failed, as
   `haarness run --wait-background` does. When the first turn fails, what it had started is stopped
   and named in the error. A one-shot schedule then disables itself.

A run writes its lock again every minute, so however long it runs, the next run does not take the
lock for an abandoned one.

Credentials are read from the normal `haarness auth` store or the environment when the run starts;
none are written into the native entry.

## Code and tests

- `crates/haarness-scheduler`: triggers and natural phrases, the job store, run history and locks,
  and the launchd, systemd and Task Scheduler backends. `cargo test -p haarness-scheduler`.
- `crates/haarness-cli/src/main.rs`: the `schedule` commands and the trigger path above.
- `scripts/test-scheduled-run-waits.py`: what a run waits for and stops (its subagents, workflows and
  commands, never a person's), through a real daemon with the echo provider.
- `crates/haarness-server/src/tools.rs`: the model's `schedule` tool. `cargo test -p haarness-server schedule`.
- `crates/haarness-cli/src/management/ui.rs`: the `schedules` management form used by the TUI and Desktop.
