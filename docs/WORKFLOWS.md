# Dynamic workflows

How workflows are discovered, run and stored, the native and portable APIs a workflow
script uses, its lifecycle and events, and its trust boundary. For workflow authors and
for contributors changing the workflow runtime (`crates/haarness-workflows`,
`crates/haarness-workflow-catalog`).

> **User guide:** [Run and write workflows](https://haarness.ai/docs/cli/workflows).

Haarness discovers reusable JavaScript and TypeScript workflows, executes each
run in a dedicated child session, and stores the source snapshot, arguments,
phase, logs, child agents, result, and lifecycle state in SQLite. Runs survive
client disconnects. A process restart marks an in-flight run `interrupted`; it
can then be inspected, deleted, or resumed from the beginning against its
original source snapshot.

## Discover and run

Definitions are discovered in this order (`WorkflowCatalog::for_workspace`):

1. `.haarness/workflows`, then the legacy `.opencode/workflows`, in the
   selected folder and then each parent up to the Git worktree root (the
   nearest folder first)
2. the global workflow folder, `workflows` in the global configuration folder:
   `$HAARNESS_DATA_DIR/config/workflows` or `$HAARNESS_HOME/config/workflows`
   when set, otherwise `$XDG_CONFIG_HOME/haarness/workflows` or
   `~/.config/haarness/workflows` on every OS
3. the legacy global `~/.opencode/workflows` and `~/.config/opencode/workflows`
4. enabled plugin workflow roots

The first project/global definition with an invocation name wins. Plugin
definitions are namespaced as `plugin-name:workflow-name`. A plugin may use
`workflows` in `.claude-plugin/plugin.json` to name one relative file or an
array of relative files/directories (default `./workflows/`); every path must
start with `./` and stay inside the plugin root.

Older versions read global workflows from the platform folder instead
(`~/Library/Application Support/dev.haarness.haarness/workflows` on macOS,
`%APPDATA%\haarness\haarness\config\workflows` on Windows). The first start of
a version that reads the global folder copies that folder's workflows into it
once (marker `.workflows-v2`), so the workflows found are the ones found before.
What the global folder held until then, never read on those systems, is kept
beside it as `workflows-before-move`; the old folder is kept but no longer read.
Nothing moves where the two are the same folder, on Linux or through a link. A
copy that fails prints a warning and is tried again on the next start.

In the TUI, `/workflow` (aliases `/workflows`, `/wf`) opens the searchable
definitions-and-runs dashboard. In the TUI and Desktop, every discovered
definition is also registered as its own searchable slash command. For
example, invocation `audit` runs as `/audit`; a plugin invocation such as
`shipping:audit` runs as `/shipping:audit`. Native plain text becomes
`{ "input": "..." }`. Portable input preserves valid JSON values and wraps
non-JSON text in the same `input` object.

The server owns command resolution, case-insensitively. Reserved `/workflow`
wins first, configured prompt commands win over a colliding direct workflow
name, and discovered workflows fill the remaining names. Use
`/workflow run <name>` to explicitly run a workflow hidden by such a collision.
The management commands:

```text
/audit check the server
/audit {"scope":"server"}
/workflow audit
/workflow audit {"scope":"server"}
/workflow run audit check the server
/workflow inspect 019...
/workflow pause 019...
/workflow resume 019...
/workflow cancel 019...
/workflow delete 019...
```

Ultracode can be enabled durably with `/ultracode`.
A standalone, case-insensitive `ultracode` keyword in an ordinary request
enables it for that turn only; identifier lookalikes and the `/ultracode`
command itself are deliberately excluded from keyword detection.

The equivalent CLI is available without opening the TUI:

```sh
haarness workflow list
haarness workflow source audit
haarness workflow run audit --args '{"scope":"server"}'
haarness workflow run audit --agent review --model openai/gpt-5.6 --variant high
haarness workflow run audit --detach
haarness workflow run audit --timeout-ms 600000 --json
printf 'check the server' | haarness run --workflow audit --workflow-arg reviewers=3
haarness workflow runs --limit 20
haarness workflow inspect 019...
haarness workflow pause 019...
haarness workflow resume 019...
haarness workflow cancel 019...
haarness workflow delete 019...
```

`workflow run` waits for the run to finish unless `--detach` is given;
`--timeout-ms` stops waiting while the run goes on on the server, and `--json`
prints the whole run record. `--agent`, `--model` and `--variant` set
workflow-wide defaults for the run's child agents. `workflow runs` lists 50
recent runs by default (`--limit` 1–500).

The model-facing `workflow` tool can read, start, wait for, inspect, create,
cancel, pause, resume, and delete runs. Creation writes
`.haarness/workflows/<name>.ts` in the project (the name a lowercase slug of
letters, digits, `-` and `_`), never overwrites an existing file, and follows the
normal workflow and edit permission decisions. Haarness
advertises this tool to a primary model turn only when the request contains a
standalone `workflow`/`workflows` word, the turn explicitly sets
`tools.workflow=true`, or Ultracode is active. Ordinary child agents do not
inherit implicit access; an author must opt them in explicitly. A catalog
definition's `whenToUse` helps choose a workflow after this gate opens and
never opens the gate by itself. Direct `/workflow`, discovered slash-command,
and `haarness run --workflow` execution bypass this model-facing gate.

The tool's `name` is a workflow's invocation as `read` lists it, so a plugin's
workflow is `plugin-name:workflow-name`. Asked for by its bare name, it is not
found, and the error names the invocation to use: "workflow audit (a plugin's
workflow is invoked as plugin:name: shipping:audit)". The HTTP API and `ctx`
nested workflows report the same.

## Native workflows

Native workflows support the workflow plugin shape and ordinary imports:

```ts
import { workflow } from "@haarness-ai/plugin"

export default workflow({
  name: "audit",
  description: "Fan out a focused repository audit",
  whenToUse: "When independent review tracks should be reconciled",
  phases: [
    { title: "scan", model: "openai/gpt-5.6", variant: "high" },
    { title: "verify" },
  ],
  arguments: {
    scope: { type: "string", default: "." },
    reviewers: { type: "number", default: 2 },
  },
  async run(args, ctx) {
    ctx.setPhase("scan")
    ctx.setStatus({ scope: args.scope })
    const reports = await ctx.parallel([
      () => ctx.agent({ prompt: `Review ${args.scope} for security issues`, label: "security" }),
      () => ctx.agent({ prompt: `Review ${args.scope} for reliability issues`, label: "reliability" }),
    ], { concurrencyLimit: args.reviewers })

    ctx.setPhase("verify")
    const verifier = await ctx.agent({ prompt: `Reconcile: ${reports.map(r => r?.text).join("\n")}` })
    const correction = await verifier.followup("Return only verified findings")
    await verifier.close()
    ctx.log("verification complete")
    return correction?.text
  },
})
```

`ctx.agent({ schema })` is enforced through the provider request, not only by
prompt wording: Haarness exposes a required `StructuredOutput` tool whose input
schema is the requested JSON Schema on OpenAI, Gemini, older Claude models and
tool-capable OpenAI-compatible providers. Claude models with adaptive thinking,
directly or through a Haarness router, use Claude's native JSON output instead,
because Opus 5.5, Sonnet 5.5 and Fable 5.1 refuse a forced tool choice. The captured value is serialized
canonically and validated again after the turn before the workflow receives
`result.data`. Ordinary agent turns and schema-free workflow agents keep their
normal text/tool behavior.

`@haarness-ai/plugin` is a runtime-provided compatibility module; workflows do
not need to install a JavaScript package. The same loader accepts
legacy plugin module aliases, as well as a default
`{ meta, run }` export. Metadata is parsed statically and must use literal
values; discovery never executes a workflow.

Catalog and inline inspection also report static capability findings for
native source without executing it. Findings cover restricted Node filesystem,
process, and network built-in imports/requires; non-literal dynamic imports or
requires; direct `fetch(...)`; and sensitive `Bun.spawn`, `spawnSync`, `write`,
`file`, and `$` access. The analysis is syntax-tree based: names in comments or
strings, `process.env`, safe modules, qualified `client.fetch`, and unrelated
`Bun` members are not findings. Findings are visibility for arbitrary native
code, not a sandbox or an execution-time denial.

## Restricted portable workflows

For generated or one-off workflows, prefer the restricted portable form. It
starts with a literal `meta` export and places the executable body after it:

```js
export const meta = {
  name: "portable-audit",
  phases: ["scan", "verify"],
}

phase("scan")
const reports = await parallel([
  () => agent({ prompt: "Inspect authentication", label: "auth" }),
  () => agent({ prompt: "Inspect filesystem boundaries", label: "filesystem" }),
], { concurrencyLimit: 2 })

phase("verify")
const manifest = await tool("read", { path: "Cargo.toml" })
log("verification inputs ready")
return { reports, manifest }
```

Portable bodies expose `args`, `phase`/`setPhase`, `status`/`setStatus`, `log`,
`agent`, `tool`, `shell`, `workflow`, `parallel`, and `pipeline`. Agent replies
support `followup` and `close`. Parallel execution preserves input order, caps
fan-out, and returns `null` for a failed branch after logging the failure. A
portable run has at most 16 agents running at once (the rest queue) and 1,000
agents in all.
Filesystem, process, network, module-import, dynamic-code, and nondeterministic
globals are unavailable to the portable body. Host operations still go through
Haarness and therefore retain tool permissions, hooks, snapshots, transcript
items, cancellation, and workflow/agent ceilings.

Human questions from inside workflow code are deliberately unsupported; the
runtime fails explicitly instead of silently blocking a background run.

## Context API

Native `ctx` provides:

- `runID`, `setPhase(name)`, `setStatus(object)`, and `log(value)`
- `agent(input)` with prompt, agent, model, variant, phase, label, tool ceiling,
  skills, files, JSON Schema, `onError`, and a native `AbortSignal`; replies have
  `text`, `data`, `followup(input)`, and `close()`
- `tool(name, args, options)` and `shell(command, options)`
- `workflow(name, args)` for nested durable workflows
- `parallel(tasks, { concurrencyLimit })` and `pipeline(items, ...stages, options)`

`tool` and `shell` accept a timeout. `tool` and `agent` support the documented
`onError: "null"` behavior. Phase model/variant defaults apply when an agent
call does not override them. An agent's explicit `phase` wins over the current
run phase, and nested workflows resolve phase metadata from their own
definition. Undeclared phase names produce a durable warning rather than
disappearing.

Requested skills are loaded deterministically through the trusted skill tool
before the child turn. Text files and directories are materialized through the
ordinary permission-, hook-, ceiling-, cancellation-, and truncation-aware
`read`/`list` tools; PDF, image, audio, and video paths become native media
attachments.
Missing or untrusted skills, denied/out-of-workspace paths, and cancellation
fail before child dispatch. Follow-ups may supply their own tools, skills,
files, schema, and `onError` while retaining the original agent/model/variant.

Tool/API timeout values use milliseconds and are clamped to a 24-hour maximum.

Budget accessors are present for source compatibility, but Haarness
does not yet impose a separate workflow-level monetary/token budget. Provider
usage and the ordinary agent/tool limits still apply.

Agents started together, as `parallel()` does, share one prompt prefix (tools
and system prompt). Where a provider writes its prompt cache explicitly
(Anthropic, a custom `anthropic-messages` endpoint, and Claude models through
the Haarness Cloud router), a cache entry becomes readable only once the
response to the request that wrote it begins. The first child request for a
prefix therefore goes alone, and its siblings' first requests wait until its
response starts streaming, so they read the prefix (0.1× input price) instead
of each writing it again (1.25×). A sibling waits at most 20 seconds from the
leader's start; a leader that fails, ends empty or is cancelled releases it at
once, and its own cancellation ends the wait. Only the timing moves: request
bytes are unchanged. Providers that cache automatically (OpenAI, Gemini) and
Bedrock are never held back.

## Lifecycle and events

Run states are `running`, `completed`, `failed`, `cancelled`, `paused`, and
`interrupted`. Pause and cancel terminate the active child process and its host
calls. Resume is intentionally restart-from-beginning; it does not pretend that
an arbitrary JavaScript stack can be checkpointed safely. Completed runs keep
their immutable SHA-256 source identity and ordered logs.

Lifecycle retries are idempotent: cancelling an already-cancelled run, pausing
an already-paused run, or resuming an already-running run returns its current
state without duplicating lifecycle logs. Other invalid state transitions still
fail with a conflict. Each active run also holds a cross-process lease. Startup
recovery is workspace-scoped and interrupts only running rows whose lease is no
longer held; a second Haarness process cannot pause, cancel, delete, resume, or
recover an execution owned by the first process.

REST clients use the workflow endpoints documented in [API.md](API.md).
`workflow.updated`, `workflow.log`, and `workflow.deleted` SSE events update
open dashboards without polling. Completion notifications are scoped to the
conversation that launched the run, while the dedicated workflow child session
keeps its tools and subagents inspectable.

A chat that launched a background run is owed its result: it is listed as
working, and is not reported done (to the apps, notifications or phones) until
the run ends, even before its first turn or after a stop. A paused run owes its
chat nothing until it is resumed; a run cancelled with its chat is owed to
nobody from that moment. A run's own threads and agents never show as chats.
Chats started inside a workflow do not fire `SessionStart` hooks, so a
workflow's agents send their first prompt without waiting for them. A run that
fails on a refusal reports the refusal's own words, as a subagent does.

## Security boundary

Native TypeScript/JavaScript is arbitrary code. Haarness clears the child
environment, isolates IPC, bounds frames/output/time, and terminates the process
group, but those controls are **not an operating-system sandbox**. Project workflow definitions load by default. `--disable-project-config` or
`HAARNESS_DISABLE_PROJECT_CONFIG=1` excludes them and disables inline native
workflows. Installed global and enabled plugin sources remain available.

Portable workflows run in a denied-globals VM with static validation and a
parent watchdog. That reduces the executable surface, but any `tool`, `shell`,
agent, or nested-workflow request still carries real application authority and
is checked by the same permission system as an interactive turn. Portable
starts and resumes always evaluate the immutable stored source snapshot; the
original installed path is only a diagnostic filename, so changing or deleting
that entry cannot silently change a durable run. Native workflows retain strict
installed-entry equality because their ordinary relative imports resolve from
that trusted module path.

A workflow's direct `tool` and `shell` calls (`ctx.tool`, `ctx.shell`) meet the
same rules as the agent's and run `PreToolUse` hooks, but no
`PermissionRequest` hooks. In a chat in Auto, the reviewer answers them as it
answers the agent's. A workflow started by a turn that approves its own
requests (a schedule's own run, `haarness run --yolo`, ACP, a `--yolo` terminal
UI) approves its direct calls once too, also after that turn ends, and in a
scheduled run a request that always asks is refused at once instead of
waiting. A workflow a person's later turn starts in a scheduled chat or its
fork is asked as usual. See [Automatic approvals](AUTO-APPROVALS.md#the-server-keeps-the-mode).
