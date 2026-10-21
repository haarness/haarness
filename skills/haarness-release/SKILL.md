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
  filenames and published versions are immutable: use a fresh preview version per attempt. The
  service keys an upload by version, product and file name on **every** channel: once any
  publish of a version has carried a file, every later publish of that version must carry the
  same bytes. A preview publish of the real release version fixes its packages for stable too,
  so do it only with the final, signed and notarized files.
- Commit before a publishable build. `release.py collect --allow-dirty` is local proof only and
  its index says so; never call a dirty build an exact SHA release.
- The workflow builds all six platforms and publishes only when every one passed. Never publish
  a partial matrix to stable: downloads read only the channel's newest release, so a platform
  missing from it answers "release platform not found" instead of falling back to an older one.
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

First, on every platform, the JavaScript the CLI embeds and Desktop bundles, in `release.yml`'s
order: the extension's typecheck needs the built SDK (`@haarness/sdk` is a `file:` dependency)
and the driver's dependencies, and `prepare.mjs` builds the SDK again. Without
`HAARNESS_EMBED_BROWSER_EXTENSION` (absolute path) the CLI builds fine but ships no Haarness for
Chrome; only `test-release-package.py` notices.

```sh
npm --prefix sdks/typescript ci && npm --prefix sdks/typescript run build
npm --prefix packages/browser-driver ci
npm --prefix apps/browser-extension ci && npm --prefix apps/browser-extension run build
export HAARNESS_EMBED_BROWSER_EXTENSION="$PWD/apps/browser-extension/dist/chrome"
```

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

### Without GitHub Actions (local cross-builds)

When the workflow cannot run (a blocked billing account), two machines cover all six platforms.
Same tag, same Bun 1.3.14 per target (checked against Bun's `SHASUMS256.txt`), same steps as above:

- **A Mac** builds both macOS architectures with `VERSION=<v>
  ~/work/haarness/haarness-deploy/scripts/release-macos.sh`: a clean worktree of the tag in
  `~/work/haarness/release-<v>/haarness`, `--target aarch64-apple-darwin` and
  `x86_64-apple-darwin`, then `package -- --mac --arm64` / `--x64` with `bun-darwin-aarch64` /
  `bun-darwin-x64` (`prepare.mjs` runs the x64 Bun under Rosetta to check its version), and one
  index per architecture in `release-<v>/dist/macos-*`. It signs and notarizes as Signing
  describes and fails closed: no Developer ID or notarization key, or a package Gatekeeper does
  not call "Notarized Developer ID", stops the build (`ALLOW_UNSIGNED=1` builds ad hoc, the
  owner's explicit call only). It holds the build lock; the run takes about 45 minutes, most of
  it Apple's notarization.
- **One Linux x64 machine** builds the rest (`haarness-deploy/scripts/release-linux-windows.sh`
  on the Ubuntu box; haarness-deploy's README, "Binary releases", has the exact commands):
  `cargo zigbuild` for both Linux targets (`.2.17`),
  `cargo xwin build` for `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`, `package --
  --linux --x64` / `--arm64` (electron-builder fetches the arm64 Electron), and each NSIS
  installer in the Wine container as in section 6. `prepare.mjs` runs the bundled Bun to check
  its version, so the arm64 Linux package needs `qemu-user-static` and `libc6-arm64-cross` with
  `QEMU_LD_PREFIX=/usr/aarch64-linux-gnu`; the same setup runs the arm64 CLI, and Docker's
  `--platform linux/arm64` gives an arm64 distribution for the smoke turn.
- Add Rust targets from inside the checkout (`rustup target add …`): run elsewhere, they go to the
  default toolchain instead of the one `rust-toolchain.toml` pins, and the build fails with
  "can't find crate for `core`".
- Build from a checkout whose HEAD is the tag commit and whose tracked files are clean:
  `collect` records both. A `git archive` copy has no `.git`; ship a `git bundle` of the tag and
  `git fetch` it instead of `git init`-ing a new history (its commit would not match).
- A cross build cannot run its binary, so label it:
  `release.py collect --binary <target binary> --os linux --arch arm64 --version <v>
  --desktop-dir apps/desktop/release --out dist/releases/linux-arm64`, one `--out` per platform
  (each run writes one `index-*.json`); `publish` refuses indexes whose version or source commit
  differ.
- The CI-only gates (stable signing, `cargo test`, the installer and package checks) do not run on
  their own: run them, and an unsigned stable release is the owner's explicit decision, stated in
  the final proof.

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
WIN_CSC_LINK, WIN_CSC_KEY_PASSWORD. Stable packaging fails closed without them, and each OS
reads only its own certificate (a Mac never falls back to WIN_CSC_LINK). APPLE_API_KEY holds the
App Store Connect `.p8` key itself (its PEM, or base64 of it): the workflow writes it to
`$RUNNER_TEMP/AuthKey_<key id>.p8` and passes that path, which is what electron-builder and
`@electron/notarize` expect in `APPLE_API_KEY`. Locally, set `APPLE_API_KEY` to the file's path,
never its contents.

**macOS, locally (`release-macos.sh`).** The identity is "Developer ID Application: STACK FORWARD
LLC (J63GND4Q8A)" (expires 2027-11-19); its `.p12` and password, and `notarization.env`
(`APPLE_API_KEY` = path of the App Store Connect `.p8`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER`) are
in `~/.config/haarness-deploy/apple/`, outside every repository. The script imports the `.p12` into
a keychain of its own and passes `CSC_NAME`, because electron-builder 26 given the `.p12` as
`CSC_LINK` hands its password to `security set-key-partition-list` and prints that command,
password included, when it fails. electron-builder signs, notarizes and staples the **app** (the
ZIP carries it) but leaves the **DMG** unsigned, so a DMG downloaded in a browser would open with
a Gatekeeper prompt. The script then signs the DMG (`codesign --timestamp`), notarizes it
(`xcrun notarytool submit --wait`), staples it, and only then runs `collect`, which records the
final SHA-256. The gate, per architecture, before collect:

```sh
spctl -a -vv -t open --context context:primary-signature Haarness-<v>-mac-<arch>.dmg
xcrun stapler validate Haarness-<v>-mac-<arch>.dmg
# and the app inside the mounted DMG and inside the extracted ZIP:
spctl -a -vv -t exec Haarness.app && xcrun stapler validate Haarness.app
```

Each `spctl` must say `accepted`, `source=Notarized Developer ID` and the team `J63GND4Q8A`.

The standalone CLI (`haarness-macos-<arch>`) is a download of its own. Linked, it carries only
the linker's ad-hoc signature, which Gatekeeper rejects for a browser-downloaded binary (curl
installs add no quarantine, so `install.sh` never noticed). The script signs it with the
Developer ID and hardened runtime, checks that it still starts (x64 through Rosetta), and
notarizes it as a zip. A bare binary cannot be stapled: Gatekeeper fetches its ticket online
when a quarantined copy first runs. `spctl -t exec` and `codesign --test-requirement '=notarized'`
do not look tickets up, so they say "Unnotarized Developer ID" even for a notarized CLI. Prove it
with the lookup Gatekeeper makes instead (`FOUND` with a `signedTicket` field; an ad-hoc binary
gives `NOT_FOUND`):

```sh
h=$(codesign -dvvv haarness-macos-arm64 2>&1 | sed -n 's/^CDHash=//p')
curl -sS -X POST -H 'Content-Type: application/json' -d "{\"records\":[{\"recordName\":\"2/2/$h\"}]}" \
  https://api.apple-cloudkit.com/database/1/com.apple.gk.ticket-delivery/production/public/records/lookup
```

Releases up to 0.1.3 shipped an ad-hoc CLI; their bytes are fixed.

**macOS in the workflow: open item.** `release.yml` notarizes the app but does not seal the DMG.
electron-builder deletes its temporary keychain when packaging ends, so a later `codesign` there
needs the identity again (its own keychain as the script does, or `--config.dmg.sign=true` during
packaging), then notarytool, stapler and the gate above. Add and prove it once Actions runs.

New Windows code
signing certificates need hardware keys (plan for a cloud signing service); the standalone
`haarness-windows-*.exe` is not signed today, so SmartScreen warns on browser downloads (the
PowerShell installer unblocks verified bytes).

**Open launch item: Desktop has no update feed.** Installed Desktop apps check for updates only
when `HAARNESS_UPDATE_URL` is set in their environment at runtime
(`apps/desktop/src/main/index.ts`, the `update` handler); nothing sets it, no release publishes
an electron-updater feed (`latest*.yml`), and macOS auto-update would also need the signed ZIP.
Until a feed exists and the packaged app knows its URL, every Desktop upgrade is a manual
download: say so in the final proof and on the download page, and do not claim auto-update.

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

Production (haarness.ai): `~/work/haarness/haarness-deploy/scripts/publish-release.sh <channel>
~/work/haarness/haarness <dist>/*/index-*.json` opens the SSH tunnel, reads the internal token
from the vault and runs the same `publish`; the receipt lands next to the indexes. To prove one
platform's packages end to end without touching stable, publish them to `preview` (only with
the final bytes, see Ground rules), then install with `HAARNESS_CHANNEL=preview`.

**Uploads bypass Cloudflare.** haarness.ai is proxied by Cloudflare, whose Free and Pro plans
cap request bodies at 100 MB (documented; larger uploads get 413 or a reset); Desktop packages
are larger. Publish to an
origin that reaches the Cloud API directly, such as an SSH tunnel to its in-cluster service
(`--url http://127.0.0.1:<local port>`; loopback HTTP is allowed). Downloads have no such limit,
so the public verification still goes through `https://haarness.ai`. The same applies to the
workflow's `HAARNESS_RELEASE_URL`: pointing it at `https://haarness.ai` fails the publish job.

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
  HAARNESS_CHANNEL=preview HAARNESS_INSTALL_DIR="$PWD/dist/releases/installed-cli" \
  HAARNESS_DESKTOP_DIR="$PWD/dist/releases/installed-desktop" sh
./dist/releases/installed-cli/haarness --version
./dist/releases/installed-cli/haarness upgrade --channel preview --release-url http://127.0.0.1:19080
```

By default the installers put in the CLI, then Desktop (`--no-desktop` / `HAARNESS_NO_DESKTOP=1`
skip it, `HAARNESS_PRODUCT=cli|desktop` installs one; `haarness upgrade` passes `cli`); a Desktop
failure is a warning, not a failed CLI install, and Linux without `DISPLAY`/`WAYLAND_DISPLAY`
skips Desktop. Destinations: CLI `~/.local/bin` (`HAARNESS_INSTALL_DIR`); macOS Desktop
`/Applications` when writable, else `~/Applications` (an app already there is updated in place;
`HAARNESS_DESKTOP_DIR`); Linux Desktop `~/.local/bin/Haarness.AppImage` plus a `haarness-desktop`
command, menu entry and icon under `~/.local/share`; Windows CLI `%LOCALAPPDATA%\Haarness\bin`,
added to the user PATH unless `HAARNESS_NO_MODIFY_PATH=1` (`haarness upgrade` sets it), and
Desktop through its NSIS setup wizard, which the script waits for (`/S` exited 2 without
installing under Wine; prove it on real Windows before switching to it). The installer checks SHA-256
before replacing or running anything; macOS clears quarantine (`/usr/bin/xattr -dr`) only on the
verified staged copy and signs it ad hoc only if its signature does not verify, so an unsigned
release opens without a Gatekeeper prompt (a browser-downloaded DMG still gets one); Windows
unblocks verified bytes. On a terminal each download is named and shows curl's progress bar.
`haarness desktop` at a terminal opens the installed Desktop, installing it first if missing.

- **macOS**: launch the installed app (not a mounted DMG, not the development Electron) and run
  `HAARNESS_DESKTOP_EXECUTABLE=…/Haarness.app/Contents/MacOS/Haarness npm --prefix apps/desktop
  run test:e2e -- e2e/desktop.spec.ts --grep 'real workspace: conversations, files, review and terminal'`.
  Then do what a browser does: download the DMG from the public URL, mark it
  (`xattr -w com.apple.quarantine "0083;<hex time>;Safari;<uuid>" Haarness.dmg`) and run the
  Signing gate on it and on the app inside. After `install.sh`, `codesign -dvv` on the installed
  app must still name the Developer ID: the installer signs ad hoc only when a signature fails
  to verify.
- **Linux** (a real machine, not only containers): `sudo apt install ./Haarness-<v>-linux-x64.deb`
  (dependencies resolve; `/usr/bin/haarness-desktop`; `xdg-mime query default
  x-scheme-handler/haarness` names `haarness-desktop.desktop`). Run Electron tests as a non-root
  user in a desktop session: `scripts/linux-desktop-session.sh` (D-Bus, unlocked gnome-keyring,
  `XDG_CURRENT_DESKTOP`, Xvfb) with `HAARNESS_DESKTOP_EXECUTABLE=/opt/Haarness/haarness-desktop`
  and `loginctl enable-linger <user>` so schedules reach `systemctl --user`. Playwright's loader
  forces `--password-store=basic` on the development Electron; the session script exports
  `HAARNESS_DESKTOP_PASSWORD_STORE=gnome-libsecret`, which test runs apply, so remote-server specs
  use the keyring there too (outside the script they fail on `basic_text`, as Desktop refuses
  plain-text credentials). Start the AppImage without libfuse2 through `haarness-desktop`.
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
- Signing and notarization state, CI state, and anything left unverified or skipped. For macOS,
  the gate's `spctl` verdicts on the DMG, the app in the DMG and the app in the ZIP, from the
  public download with quarantine set, and the CLI's ticket lookup.
