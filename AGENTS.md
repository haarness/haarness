# Repository skills

Before building, running, testing, packaging, or cleaning build artifacts for Haarness CLI, TUI, server, SDKs, or Desktop, read and follow [.skills/haarness-build-run/SKILL.md](.skills/haarness-build-run/SKILL.md).

Use the skill's smallest relevant development loop: hot reload for Desktop edits, focused Rust builds/tests for backend work, release packaging only when needed. Keep incremental caches warm; do not add automatic cleanup to every build. Preserve existing source changes, sessions and unrelated running applications.

## Disk space

Work here grows without bound unless someone cleans up: every worktree has its own `target/` (20–50 GB once tests are built), and release, Docker and test runs leave large folders behind. Fast development comes first: cleanup removes only what nothing uses any more, never during a build or test run, and never the caches of a checkout someone is working in (the main checkout's `target/` and `node_modules` stay warm).

- **When:** at the end of a task that created worktrees, release folders or images (clean up after yourself), and when `df -h .` shows less than 20% free or more than ten worktrees exist (then everything finished below, not only your own). Not before or after each build.
- **Finished worktrees** (`git worktree list`): remove one with `git worktree remove <path>`, and its branch with `git branch -d`, only when the branch is merged into `main` (`git branch --merged main`), `git status` there is clean and no process has files open in it (`lsof +D <path>`). Never remove the main checkout, a worktree with uncommitted or unmerged work, or one another session is working in; when unsure, leave it and name it in your report.
- **Checkouts that stay:** leave their debug caches alone; only when the volume is under 20% free, and only in a checkout nobody has built in for a week, run `python3 scripts/clean-build-artifacts.py` to preview, then with `--apply` (see the skill). Release and cross-compile outputs (`target/release`, `target/<triple>`, `apps/desktop/release`) and `apps/desktop/test-results` can go once no packaging or test run needs them; the next dev build does not use them.
- **Release folders** (`../release-<version>`): once a release is published, remove its build worktree and download caches, keeping `dist/*/index-*.json` and the receipt; keep the newest published release's files until the next one is out; delete dry-run folders once what they found is recorded.
- **Docker:** remove only what Haarness builds made: build cache older than a week (`docker builder prune --filter until=168h`), Haarness image tags other than the versions deployed (`environments/*/version.yml` in haarness-deploy names them), and `haarness-release-*` volumes when no release build runs. Never prune all images, containers or volumes: other projects run on the same Docker.
- **Round state folders** (`../*-state`): once a round is recorded in `docs/records/`, delete their binary copies, test data and temporary folders; keep the text reports and screenshots.
- Say in your report what you removed and how much it freed (`df -h .` before and after).
