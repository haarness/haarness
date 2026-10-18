---
name: haarness-release
description: Build, verify, publish and present Haarness CLI and Electron Desktop releases for macOS, Linux and Windows through the Cloud release API — per-platform builds (glibc-pinned Linux, static-CRT Windows), DMG/ZIP, AppImage/deb and NSIS packages, curl and PowerShell installers, checksums, quarantine, the download page, README/profile/docs install sections, SDK proof, rollback and a final proof report. Use when building, publishing, shipping or install-testing a release; excludes npm and Docker publication.
---

# Haarness binary releases

## Ground rules

- Start in the native repository (Cargo.toml and apps/desktop) and read
  [build/run guidance](../../.skills/haarness-build-run/SKILL.md). Preserve existing work and
  unrelated app instances. Use an isolated Cloud checkout if its primary is dirty.
- One version for the CLI and Desktop (Cargo workspace = `apps/desktop/package.json`). Uploaded
  filenames and published versions are immutable: use a fresh preview version per attempt.
- Commit before a publishable build. `release.py collect --allow-dirty` is local proof only and
  its index says so; never call a dirty build an exact SHA release.
- The workflow builds all six platforms and publishes only when every one passed. Never publish
  a partial matrix to stable.
- The installers live twice: `install.sh`/`install.ps1` here and
  `haarness-cloud/assets/install.{sh,ps1}`, which the Cloud serves. Keep them identical.
- Do not limit testing to this recipe. Chase undocumented paths and failures, fix causes, add
  regression checks, and say which proof is local, which is CI and which is public.
- A blocked CI billing account means the cross-platform jobs did not run: that is unverified,
  not green.

## Platform matrix

| Platform | CLI build | Desktop | Published files | Platform checks |
| --- | --- | --- | --- | --- |
| macOS arm64, x64 | `cargo build --release --target <triple>` | DMG, ZIP | `haarness-macos-<arch>`, `Haarness-<v>-mac-<arch>.{dmg,zip}` | package, quarantine, packaged Electron tests |
| Linux x64, arm64 | `cargo zigbuild --release --target <triple>.2.17` | AppImage, deb | `haarness-linux-<arch>`, `Haarness-<v>-linux-<arch>.{AppImage,deb}` | `test-release-linux.py` |
| Windows x64, arm64 | `cargo build --release --target <triple>` (static CRT) | NSIS EXE | `haarness-windows-<arch>.exe`, `Haarness-<v>-win-<arch>.exe` | `test-release-install-ps1.py`, package |

Every later step reads the one CLI binary through `HAARNESS_BIN`; Desktop packaging copies it
(`apps/desktop/scripts/prepare.mjs`) and `collect` publishes it.

## 1. Pre-build gate

1. Native and Cloud Git status, HEAD, remotes, current manifests; versions match.
2. `cargo test --workspace`, Desktop `npm run typecheck` and `npm test`, SDK build/tests,
   `sh scripts/check-commercial-boundary.sh`, `python3 scripts/test-release-publish.py`.
   (`cargo clippy -- -D warnings` has failed on main since the first commit on pedantic lints;
   check that a change adds no new finding instead of reading the total.)
3. Compile the platforms you are not on. From Linux or macOS, Windows:
   `cargo xwin clippy --workspace --all-targets --target x86_64-pc-windows-msvc` (needs
   clang/lld; on Ubuntu link `clang-cl`, `lld-link`, `llvm-lib` to the llvm-14 tools). Windows-
   only dead code and `std::os::unix` in tests show up only there.

## 2. Build per platform

Rust from rust-toolchain.toml, Node 22+, **Bun 1.3.14** (never whatever Bun is first on PATH).

```sh
# macOS (and Windows, with --win and haarness.exe)
cargo build --release -p haarness-cli --target aarch64-apple-darwin
export HAARNESS_BIN="$PWD/target/aarch64-apple-darwin/release/haarness"
HAARNESS_BUN=/absolute/path/to/bun npm --prefix apps/desktop run package -- --mac --arm64
```

**Linux** never ships a binary linked against the build machine's glibc (an ubuntu-24.04
build needs glibc 2.39 and fails on Ubuntu 22.04, Debian 12 and RHEL 9). Link against 2.17, as
OpenTUI and Bun do, and bundle Bun's baseline build on x64 (it runs without AVX2):

```sh
python3 -m pip install ziglang==0.14.1 cargo-zigbuild==0.23.4
cargo zigbuild --release -p haarness-cli --target x86_64-unknown-linux-gnu.2.17
export HAARNESS_BIN="$PWD/target/x86_64-unknown-linux-gnu/release/haarness"
HAARNESS_BUN=/absolute/path/to/bun-linux-x64-baseline/bun \
  npm --prefix apps/desktop run package -- --linux --x64
```

**Windows** links the C runtime statically (`.cargo/config.toml`), so `haarness.exe` starts on a
clean machine without the Visual C++ Redistributable, and x64 bundles Bun's baseline build as
Linux does. Check it:
`llvm-readobj --coff-imports haarness.exe` lists only system DLLs (no `VCRUNTIME140.dll`,
no `api-ms-win-crt-*`). Always pass `--target`: the `windows-11-arm` runner's host toolchain
need not match the architecture it publishes.

## 3. Package checks and collect

```sh
python3 scripts/test-release-package.py              # bundled CLI, Bun, Chrome extension
python3 scripts/release.py collect --binary "$HAARNESS_BIN"
python3 scripts/test-release-install.py              # install.sh faults (macOS, Linux)
python3 scripts/test-release-install-ps1.py          # install.ps1 under pwsh, every OS
python3 scripts/test-release-linux.py                # Linux only
python3 scripts/test-release-quarantine.py           # macOS only
```

- `collect` stages checksummed artifacts and one platform index under `dist/releases`; keep it
  unchanged for idempotent retries. electron-builder names Linux packages `linux-x86_64.AppImage`
  and `linux-amd64.deb`; `collect` publishes both as `linux-x64`. Windows reports `ARM64`.
- `test-release-linux.py`: glibc floor and linked libraries; both packages carry the release
  CLI, package `haarness-desktop` with maintainer/homepage, a launcher entry owning
  `haarness://`, icons at listed sizes; `install.sh` installs the CLI and the AppImage with its
  `haarness-desktop` command, menu entry and icon; in Docker, the CLI loads OpenTUI and answers
  a turn on CentOS 7, Amazon Linux 2, Rocky 8, Debian 11 and Ubuntu 22.04 (no Git anywhere; the
  Debian family lacks ca-certificates and must get a plain message, not a startup failure).
- `test-release-install-ps1.py`: corrupt/missing checksum, plain-HTTP origin, unknown channel and
  a download that cannot run are all refused with the folder untouched; on Windows with
  `HAARNESS_BIN` the real binary installs over a previous copy.

## 4. Signing

Public macOS DMG/ZIP needs commercial Developer ID signing and notarization; ad-hoc signing is
local preview proof only. Never use unrelated employer identities. Secrets: MAC_CSC_LINK,
MAC_CSC_KEY_PASSWORD, CSC_NAME, APPLE_API_KEY, APPLE_API_KEY_ID, APPLE_API_ISSUER; Windows
WIN_CSC_LINK, WIN_CSC_KEY_PASSWORD. Stable packaging fails closed without them. New Windows code
signing certificates need hardware keys (plan for a cloud signing service); the standalone
`haarness-windows-*.exe` is not signed today, so SmartScreen warns on browser downloads (the
PowerShell installer unblocks verified bytes).

## 5. Publish through the Cloud release service

Apply Cloud migrations, build `cloud-api`, restart only the owned test API. It uses its object
storage and `HAARNESS_CLOUD_API_PUBLIC_URL` for artifact URLs; public deployment needs a
persistent S3/R2 bucket, HTTPS and DNS. Operator mutations need the internal token; account/API
credentials must fail. Keep tokens out of arguments and logs.

```sh
# HAARNESS_RELEASE_TOKEN is injected securely from the local sandbox or release secret.
python3 scripts/release.py publish --index dist/releases/index-*.json \
  --url http://127.0.0.1:19080 --channel preview --receipt dist/releases/receipt.json
```

The publisher streams each upload with length and SHA-256, publishes the set atomically, then
downloads every public artifact and verifies hash and size.

- Public: `/api/v1/releases?channel=preview`, `/api/v1/releases/history?channel=preview`,
  `/api/v1/releases/artifacts/{id}`, and
  `/api/v1/releases/{channel}/{product}/download?os=macos&arch=arm64&kind=zip` (`version` pins).
  Kinds: CLI `binary`; Desktop `dmg`/`zip`, `AppImage`/`deb`, `exe`.
- Operator: POST `/internal/releases/artifacts`, POST `/internal/releases`, POST
  `/internal/releases/{id}/promote` (same-channel rollback), DELETE `/internal/releases/{id}`
  (withdraw; never silently selects an older release). Roll back by promoting a known-good receipt.

## 6. Install and run the downloaded bytes, per platform

```sh
curl -fsSL http://127.0.0.1:19080/install.sh | HAARNESS_RELEASE_URL=http://127.0.0.1:19080 \
  HAARNESS_CHANNEL=preview HAARNESS_INSTALL_DIR="$PWD/dist/releases/installed-cli" sh
./dist/releases/installed-cli/haarness --version
./dist/releases/installed-cli/haarness upgrade --channel preview --release-url http://127.0.0.1:19080
```

Destinations: CLI `~/.local/bin`; macOS Desktop `~/Applications`; Linux Desktop
`~/.local/bin/Haarness.AppImage` plus a `haarness-desktop` command, menu entry and icon under
`~/.local/share`; Windows CLI `%LOCALAPPDATA%\Haarness\bin`, added to the user PATH unless
`HAARNESS_NO_MODIFY_PATH=1` (`haarness upgrade` sets it). The installer checks SHA-256 before
replacing or running anything; macOS clears quarantine (`/usr/bin/xattr -dr`) only on the
verified staged app; Windows unblocks verified bytes.

- **macOS**: launch the installed app (not a mounted DMG, not the development Electron) and run
  `HAARNESS_DESKTOP_EXECUTABLE=…/Haarness.app/Contents/MacOS/Haarness npm --prefix apps/desktop
  run test:e2e -- e2e/desktop.spec.ts --grep 'real workspace: conversations, files, review and terminal'`.
- **Linux** (a real machine, not only containers): `sudo apt install ./Haarness-<v>-linux-x64.deb`
  (dependencies resolve; `/usr/bin/haarness-desktop`; `xdg-mime query default
  x-scheme-handler/haarness` names `haarness-desktop.desktop`). Run Electron tests as a non-root
  user in a desktop session: `scripts/linux-desktop-session.sh` (D-Bus, unlocked gnome-keyring,
  `XDG_CURRENT_DESKTOP`, Xvfb) with `HAARNESS_DESKTOP_EXECUTABLE=/opt/Haarness/haarness-desktop`
  and `loginctl enable-linger <user>` so schedules reach `systemctl --user`. Specs that launch the
  development Electron store remote-server credentials as `basic_text` there and fail; the
  packaged app uses the keyring. Start the AppImage without libfuse2 through `haarness-desktop`.
- **Windows** (a real machine or VM): `irm …/install.ps1 | iex` in Windows PowerShell 5.1 and in
  PowerShell 7; PATH added; `haarness doctor`; `$x = haarness run --model echo/echo-1 hi`
  returns (a background server must not hold the caller's pipe); the NSIS installer, Start menu
  entry, a `haarness://` link with the app closed, and `haarness upgrade` replacing a running
  binary. Without Windows: Wine runs `--version` and `doctor` (it does not model handle
  inheritance or cmd.exe quoting), `pwsh` on Linux/macOS runs the installer tests, and the NSIS
  installer builds in a Wine container with the resources staged by hand (`prepare.mjs` stages
  the host's): `resources/haarness.exe` from `cargo xwin build`, `resources/bun.exe` from
  `bun-windows-x64-baseline.zip`, then `npx electron-builder --win --x64 --publish never`.

Run the SDK server scenarios against the **installed CLI** (`HAARNESS_BIN=…`) and an owned tmux
echo-provider chat for the TUI (tmux 3.4+; 3.2a draws ZWJ emoji apart).

## 7. Present the release

Every place that tells people how to install must list macOS, Linux and Windows with the same
commands and file kinds:

- README `## Install`: one copy-ready block for macOS and Linux, one for Windows (PowerShell),
  then the table of destinations, downloads and supported systems.
- `haarness/.github` `profile/README.md` (public): the same commands and file kinds; no links to
  private repositories before launch.
- Cloud `/download`: each OS tab shows the right hero logo and file (DMG, AppImage before .deb,
  EXE), the CLI command (sh or PowerShell) and the Desktop command. Check it against a real
  manifest, or locally with a mock API: run vite with `HAARNESS_CLOUD_API_ORIGIN` pointing at a
  server that returns a six-platform manifest shaped like `release.py publish`.
- Docs: `docs/public/start/02-install.md` and `docs/public/desktop/01-install.md`.
- Preview README rendering before pushing: `gh api -X POST /markdown` (mode gfm, repo context)
  wrapped in github-markdown-css, opened in the browser.

## 8. Regression and SDK proof

Cloud: `cargo test -p cloud-contracts --test openapi_paths` and `cargo test -p cloud-api --test
releases` with a disposable test database (a missing database skips them; that is not proof):
concurrent publishing, bad checksums, changed artifacts/versions, unpublished download denial,
invalid platforms, withdrawal and rollback, signed-manifest preservation. Re-export OpenAPI and
regenerate both SDKs after route changes. Run the [SDK release tests](../haarness-sdk-surface/SKILL.md).
Clean up test keys, hosts, temporary channels and processes; restore the prior publication on
failure. Never delete unrelated sessions, apps or build caches.

## 9. Final proof

Report, in this order:

- Version, source commit, channel, receipt/release ID.
- Per platform: file names, SHA-256 and sizes; how each was proven (native machine, CI run,
  container/Wine/cross-compile, or not at all).
- Installer results per OS (sh, PowerShell 5.1/7), upgrade, quarantine/unblock.
- Desktop test results per OS, with known environment failures named and why.
- Download page, README, profile and docs checks (screenshots or rendered previews).
- Signing and notarization state, CI state, and anything left unverified or skipped.
