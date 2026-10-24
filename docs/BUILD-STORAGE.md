# Keeping local builds small

The Cargo profile settings that keep a Haarness checkout's `target/` small, and how to clean it safely. It is for contributors building Haarness locally.

Routine Haarness builds and tests use line-table debug information and retain incremental compilation for fast edits. The sibling Haarness Router workspace uses the same settings. File/line backtraces remain available; inspecting local variables in a native debugger requires the explicit `debugger` profile.

```sh
cargo build -p haarness-cli
cargo test -p haarness-protocol
# Only when a native debugging session needs full type/variable information:
cargo build --profile debugger -p haarness-cli
```

Tests inherit the development profile, avoiding an unnecessary difference in dependency settings between `cargo build` and `cargo test`. Debug assertions and overflow checks remain enabled. Release settings are unchanged. These choices follow the [Cargo profile documentation](https://doc.rust-lang.org/cargo/reference/profiles.html).

Incremental compilation saves compiler state to speed up edits. Keep it on during rapid prototyping and reuse the same target directory, toolchain, and feature set. Reduced debug information limits the footprint without discarding those speed benefits. After removing build intermediates, the next build must recreate them once.

For an intentionally space-constrained build session, use `CARGO_INCREMENTAL=0 cargo build -p haarness-cli` (or the corresponding test command). Apply that choice consistently during the session; switching settings repeatedly can create more artifact variants. It trades some rebuild speed for less stored compiler state. Do not make cleanup a pre-build or post-build hook.

## Safe cleanup

From the Haarness repository, preview rebuildable debug intermediates:

```sh
python3 scripts/clean-build-artifacts.py
# Include the sibling Router workspace:
python3 scripts/clean-build-artifacts.py --router
# Apply the same cleanup:
python3 scripts/clean-build-artifacts.py --router --apply
```

The helper removes only the `incremental`, `deps`, `build`, `.fingerprint`, and `examples` directories inside the standard `target/debug` and optional `target/debugger` profiles. It keeps root-level runnable binaries, release builds, the packaged Desktop app, installed JavaScript dependencies, vendor archives, test reports, and all conversation/configuration data. Existing binaries remain runnable, but their removed debug objects will no longer be available for debugging until rebuilt.

The helper defaults to preview, checks Cargo cache markers, rejects symlinked cache roots, acquires Cargo's profile locks, and refuses removal if the current user's processes have files open inside the selected caches. It requires Python 3 and `lsof` on macOS/Linux. Run it when build/test processes are idle. It intentionally does not follow custom `CARGO_TARGET_DIR` locations.

Use `--incremental-only` under disk pressure to remove compiler state while retaining compiled dependencies. Normal builds will recreate incremental state; keep it warm while iterating. Do not clean after every build: that throws away reusable dependency work. Run cleanup after major toolchain/profile changes or when the target directory becomes large.

Cargo can still accumulate binaries from different feature sets, toolchains and test targets; these settings reduce the footprint rather than impose a hard disk quota. Avoid creating a fresh target directory for every test run, and use focused package tests for changes that do not require the full workspace suite. Desktop renderer tests can reuse an existing backend with `HAARNESS_BIN` when Rust code has not changed.
