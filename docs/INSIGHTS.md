# Cross-project analytics

How `haarness stats`, `haarness insights`, the TUI's `/stats` and `/insights` and the
`/v1/stats` and `/v1/insights` routes read local history, what they count and what a
report contains. For contributors changing the analytics and anyone checking a figure.

> **User guide:** [See your usage and cost](https://haarness.ai/docs/cli/stats-and-insights).

`haarness stats` and `haarness insights` read session history across every workspace
in the current local data root. Servers can be running or stopped. Analytics do not
start servers, register projects, migrate databases, load credentials or call providers.

```sh
haarness stats
haarness stats --days 7 --json
haarness stats --current-project
haarness stats --project PROJECT_ID
haarness insights --days 30 --limit 200
haarness insights --current-project --open
haarness insights --output-dir ./reports --json
```

With no filters, all recorded history is included. `--days N` selects turns started
within a rolling UTC window, including all recorded events of those turns.
`--limit N` selects the most recently active matching sessions across projects.
`--project` accepts an ID, unique name or root path; `--current-project` includes
the project's other registered workspaces. The hidden global `--data-dir` (or
`HAARNESS_DATA_DIR`) selects an isolated installation.

JSON and HTML contain project, model, tool, file, day and session breakdowns.
The HTML report also includes deterministic findings. `insights` writes a unique
private HTML file under `<data>/insights/reports` and prints its path. `--open`
opens the browser; `--no-open` and `--no-llm` explicitly describe the defaults.
`--output-dir` selects another directory. Existing reports are never replaced.

In the TUI, `/stats` shows a summary panel. `/insights` also saves HTML on the
**client's machine**, as `haarness-insights-<id>.html` (owner-only on Unix) in the system
temporary folder, and copies its path; with `--open` it also opens the report,
except over SSH. Both accept `--days N`, `--limit N`, `--current-project` and
`--project ID`. Attached clients analyze the server's history. Desktop has no
analytics view.

## Accounting and coverage

- Model attribution uses each turn's provider and model, not its thread's current model.
- Archived sessions and child agents count. Detached fork-context items do not.
- Imported turn IDs count once across databases. The copy with the most complete
  usage wins; stable database order breaks ties.
- Undo and soft deletion do not erase usage already spent or recorded edit activity.
- Cost is a **known subtotal** of recorded estimates. Unpriced records and turns
  without usage are counted explicitly; unknown prices are not silently treated as free.
- Token total is input plus output. Cache counters are separate, not added again.
- File statistics use structured tool-result diffs, including partial edits.
  They count edit activity, not a net Git diff. Shell edits and older results
  without diff metadata are outside this measurement.
- Each database is read in a consistent transaction. Running projects can advance
  between reads. Unreadable databases produce visible warnings and partial results.
- Closed WAL databases may require SQLite to initialize or reconcile journal sidecars.
  Analytics permit that housekeeping through a query-only connection, without creating
  a database, changing session records, or running schema migrations.
- All-project reports include legacy and unregistered databases. Legacy history
  cannot be attributed to a selected project. Reports show scope and deduplication counts.

Reports include project names, session titles, IDs and file paths, but exclude
prompts, tool arguments, file bodies and tool output. HTML escapes stored text,
disables scripts and external resources, and uses no external fonts or libraries.
Review names and paths before sharing. Findings describe recorded activity and
execution status; they do not infer task quality or developer productivity.

## API

- `GET /v1/stats` returns the versioned JSON report.
- `GET /v1/insights` returns `{ "summary": "...", "html": "..." }` without writing a server file.
- Query parameters: `days`, `project`, `currentProject`, `limit`.
- Both use the server's ordinary authentication and need its shared workspace
  registry (without one they answer "shared workspace history is not configured").
  Scope is the server's entire local data root. OpenAPI publishes both endpoints
  and schemas.

## Verification

```sh
cargo test -p haarness-insights
cargo test -p haarness-cli --test insights_e2e
HAARNESS_TEST_TMUX=1 cargo test -p haarness-cli --test insights_e2e
```

Accounting tests cover projects/worktrees, model changes, imports/forks, undo,
missing prices, filters, corrupt databases, HTML escaping and private files.
The CLI end-to-end test uses real isolated servers with the deterministic provider,
writes files in two projects, and reconciles CLI, REST and HTML totals.
The optional tmux run also exercises `/stats`, project filtering, and `/insights`
through the actual terminal UI. Set `HAARNESS_TEST_ARTIFACTS` to retain pane captures.
