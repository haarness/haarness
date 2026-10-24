# Reviewing large changesets

How Desktop's Review and the TUI's `/diff` stay fast on very large diffs, the review API's bounds, what Git may run during a review, and the scaling tests. For contributors working on Review, the VCS endpoints or clients that page through diffs.

> **User guide:** [Review what changed](https://haarness.ai/docs/desktop/projects-and-panels#review-what-changed).

Desktop loads the changed-file list before requesting patch bodies. Selecting a
file loads its changes; **Show every file at once** loads files near the visible
part of the review as you scroll. **Filter changed files** searches the whole
list, including files that have not been rendered yet.

Both the file list and the diff lines are virtualized. Split and unified views
retain line numbers, word highlighting, reviewed-file marks, patch copying and
inline comments. Switching layouts reveals the line with an open comment, even
when a large replacement moves that line far down the unified view. Comment
drafts survive refresh and layout changes. Changing the server, conversation or
review source clears comment drafts so they cannot attach to unrelated changes.
Loading files retain their measured space so revisiting a file does not collapse
the scroll area. File headers stay at the top of the visible review while you
scroll through their changes.

Working-tree and branch reviews read current Git data when refreshed. Last-turn
review uses the captured before/after checkpoints, so later manual edits do not
change a completed turn's diff. It shows the newest turn that changed files and
is not undone. Copy patch also works for these snapshot diffs.

The UI retains at most 24 cached file bodies and approximately 24 MiB of UTF-16
body text, in addition to the currently rendered files. It performs at most three
concurrent body requests per review generation. Old requests cannot populate a
new server or review source. File bodies that leave the cache are fetched again
when revisited. Existing server size limits still apply to individual files;
binary and truncated content is labelled explicitly. Very long lines retain
their full text but skip expensive word-level highlighting.
Transient Git failures keep the refresh and source controls available. Git
commands retain their existing ten-second timeout; an overloaded machine can
still require a retry.

The TUI gives review requests their own 120-second deadline instead of the
blanket 15-second API deadline, reports a timeout and an unreachable daemon
differently, and loads `/diff` on a worker task: the viewer opens immediately in
a **Loading review…** state, keeps repainting and streaming while the request is
in flight, and Escape closes it and cancels the request.

## API

Existing eager endpoints remain compatible:

- `GET /v1/vcs/diff?mode=working&context=4` returns bounded patches as before.
- Add `summary=true` to return changed-file metadata without patch bodies.
- Add `path=src/example.ts` to request one literal workspace-relative file.
  This works for working-tree and branch sources, tracked and untracked files,
  and filenames containing wildcard characters. URL-encode the query value.
- `GET /v1/threads/{id}/diff` retains its existing behavior.
- `GET /v1/threads/{id}/diff/review?summary=true` lists last-turn metadata.
- `GET /v1/threads/{id}/diff/review?path=src/example.ts` loads one last-turn file.

### Review bounds

`GET /v1/vcs/diff` is bounded so one request cannot return an unbounded
response or run for an unbounded time:

- at most 1000 changed files per response,
- at most 512 KiB of patch body per file,
- at most 8 MiB of patch body per response.

Tracked changes claim the file bound first, so untracked scratch in a working
tree cannot push the work under review out of its own diff. Untracked files are
read directly rather than through a `git diff --no-index` process per file, and
a file whose first 8000 bytes contain a NUL is reported as `binary` instead of
being diffed. `.gitignore`, nested ignore files and the usual excludes are
honoured by the same `git ls-files --others --exclude-standard` listing as
before.

### What a review runs

A review only reads, but Git runs programs a repository's own configuration
names (`crates/haarness-server/src/vcs.rs`). What Review lets run depends on
whether the project's configuration is trusted, as for project hooks and
extensions ([project trust](CONFIGURATION-SCOPES.md); `--disable-project-config`
or `HAARNESS_DISABLE_PROJECT_CONFIG=1` turns it off, and Desktop opens a folder
from a link untrusted).

- **Always, trusted or not:** no hooks (`core.hooksPath` is the null device; a
  diff that refreshes the index would run `post-index-change`), no file system
  monitor (`core.fsmonitor`), no signature checks (`log.showSignature`), and
  no external diffs or text conversion (`--no-ext-diff --no-textconv`). Git's
  own messages reach the review in English whatever the locale
  (`GIT_TEXTDOMAINDIR` names no folder), while the programs Git runs keep the
  person's locale, so the review recognises a message in any language and the
  diff output and file names are unchanged.
- **Trusted (the default):** everything else reads as the person's own
  `git diff`: the repository's clean, smudge and process filters run (an
  `nbstripout` or `git-crypt` user sees the same Review), a changed submodule
  is looked inside for uncommitted changes, and a partial clone fetches the
  objects a review needs from its remote.
- **Not trusted:** a folder someone else may have prepared (a copied checkout,
  an unpacked archive) runs none of those. The repository's own filters are
  emptied before the diffs that read changed files (the empty-named
  `[filter ""]` included); the standard git-lfs commands and filters from the
  user's global and system configuration still run, unless the repository
  defines a git-lfs extension, which empties them all. Submodules are listed
  when they point at another commit, without looking inside them (that runs a
  second Git with the submodule's own configuration). A partial clone fetches
  nothing: a review that needs files the clone never downloaded fails with
  `this clone has not downloaded the files the review needs, and the review
  does not download them while project configuration is not trusted; run git
  backfill (Git 2.49 or later), then try again`. Desktop's Review shows
  "Review needs files this clone hasn’t downloaded yet." with **Check again**,
  and the TUI's `/diff` says the same in one line. `git fetch` does not
  download them; `git backfill` does, as does trusting the project's
  configuration. When the filters cannot be read, a filter's name is not
  UTF-8, or this Git cannot turn one off, Git is not run and the review says
  so.

The untrusted protections need Git 2.45 or later in full. On older Git:
before 2.45 a partial clone still fetches missing objects (`GIT_NO_LAZY_FETCH`
is new in 2.45); before 2.31 the filters are emptied with `-c` arguments
instead of `GIT_CONFIG_COUNT`, and a repository whose filter name holds `=`
cannot be reviewed untrusted; before 2.26 (no `--show-scope`) the filters from
the user's own configuration are emptied too.

Creating, resetting and removing a worktree are started by the person and keep
the repository's own filters and hooks, as Git does for any checkout.

Each entry of the response is a `ReviewFile`: a `FileDiff` plus two optional,
defaulted fields, so the wire shape stays a superset of `FileDiff` and existing
clients are unaffected.

- `truncation` — `binary`, `file_too_large` or `response_too_large`, saying why
  an entry arrived without its body. `truncated` still says *that* it did.
- `omittedFiles` — changed paths the response does not list at all. The
  response is an array with nowhere else to put it, so every entry repeats the
  same count; a review that listed everything omits the field.

A single-file request (`path=`) is a deferred-detail fetch and may spend the
whole response budget on that one file.

Summary responses use the existing `FileDiff` shape with omitted bodies; omission
for this query does not imply truncation. A per-file request receives its own
response byte budget, so an earlier large file cannot consume another file's
budget. Summary counts still require Git numstat or reading snapshot contents;
this is deferred body generation and transfer, not a constant-time Git scan.

TypeScript exposes `vcsDiff({summary: true})`, `vcsDiff({path})` and
`lastTurnReview(id, {summary: true})`. Python provides the corresponding
`vcs_diff` and `last_turn_review` methods in both clients.

## Verification

The dedicated Electron suite creates real temporary Git repositories and runs
the real Haarness server. API responses are not stubbed. Only the operating
system's folder picker is replaced to select the test directories.

```sh
cargo build -p haarness-cli
npm --prefix sdks/typescript run build
npm --prefix apps/desktop run build
cd apps/desktop
npx playwright test e2e/review-scaling.spec.ts
REVIEW_FILES=10000 npx playwright test e2e/review-scaling.spec.ts
```

The suite records opening time, animation-frame gaps, long tasks and rendered
node counts in `review-scaling.json`, with screenshots and retained failure
traces. Its fixtures include a 10,000-line replacement, many changed files,
untracked files, a deletion, binary content, literal special-character names,
snapshot review and an oversized file. The file-count and line-node assertions
check rendering bounds rather than merely asserting that a spinner disappeared.
It also checks that the final line remains visible after loading and resizing,
recovers from an invalid Git index and a temporarily unavailable repository, and
returns to the empty state after committing all changes. Run performance tests
without concurrent builds or test suites to avoid distorting the measurements.

`REVIEW_BASELINE=1` runs the opening measurement without the new API and rendering
assertions, for comparison against an earlier Desktop/backend build.

### Local measurements, September 13, 2026

Observed on macOS with the production Electron renderer and debug Haarness
server. Each fixture also contains a 10,000-line replacement and four additional
changed files; these are individual runs, not portable performance guarantees.

| Fixture | Open review | Longest recorded main-thread task | Rendered diff rows |
| --- | ---: | ---: | ---: |
| 1,000 files, before | 4,454 ms | 1,924 ms | 10,000 |
| 1,000 files, after | 938 ms | 63 ms | 46 |
| 10,000 files, after | 4,000 ms | 69 ms | 44 |

The default compact review keeps the file picker hidden. The navigation tests
open it and assert fewer than 100 mounted entries while filtering and navigating
the full changeset, and fewer than 200 mounted diff rows in the all-files view.
