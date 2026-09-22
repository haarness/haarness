# Repository skills

Before building, running, testing, packaging, or cleaning build artifacts for Haarness CLI, TUI, server, SDKs, or Desktop, read and follow [.skills/haarness-build-run/SKILL.md](.skills/haarness-build-run/SKILL.md).

Use the skill's smallest relevant development loop: hot reload for Desktop edits, focused Rust builds/tests for backend work, release packaging only when needed. Keep incremental caches warm; do not add automatic cleanup to every build. Preserve existing source changes, sessions and unrelated running applications.
