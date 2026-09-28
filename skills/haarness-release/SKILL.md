---
name: haarness-release
description: Build, publish, install and verify Haarness CLI and Electron Desktop releases through the Cloud release API, including curl/PowerShell installers, checksums, quarantine, SDKs, Chrome downloads and rollback. Use for binary release and installation testing; excludes npm and Docker publication.
---

# Haarness binary releases

Start in the native repository (Cargo.toml and apps/desktop), and read
[build/run guidance](../../.skills/haarness-build-run/SKILL.md). Preserve existing
work and unrelated app instances. Use an isolated Cloud checkout if its primary is dirty.

Do not limit testing to this recipe. Investigate relevant undocumented paths, edge
cases and failures discovered while testing. Fix causes, add useful regression checks,
and distinguish local proof from platform/public deployment proof.

## Freeze and build

1. Check native and Cloud Git status, HEAD, remotes and current manifests. Use a fresh
   preview version; an uploaded filename and a published version are immutable.
2. Match Cargo workspace and Desktop package/lock versions. Commit source before a
   publishable build. `release.py collect --allow-dirty` is only for explicit local
   proof, and its index records that fact. Never label a dirty build as an exact SHA release.
3. Install dependencies only when missing or changed. Use Rust from rust-toolchain.toml,
   Node 22+, and **Bun 1.3.14** (not whichever Bun happens to be first on PATH).

```sh
cargo build --release -p haarness-cli
HAARNESS_BIN="$PWD/target/release/haarness" HAARNESS_BUN=/absolute/path/to/bun \
  npm --prefix apps/desktop run package -- --mac --arm64
python3 scripts/test-release-package.py
python3 scripts/release.py collect
```

Use `--linux --arm64`, `--linux --x64`, `--win --arm64`, `--win --x64`, or
`--mac --x64` on the corresponding native runner. The CLI is a standalone executable;
Desktop packages include that CLI, Bun and license notices. macOS provides ZIP/DMG,
Windows NSIS EXE, Linux AppImage/deb. `collect` stages checksummed artifacts and one
platform index under `dist/releases`. Keep this index unchanged for idempotent retries.
The workflow builds all six platforms and publishes only after every build succeeds.

Public macOS DMG/ZIP distribution needs commercial Developer ID signing and
notarization; ad-hoc signing is local preview proof only. Never use unrelated employer
identities. Configure release environment secrets MAC_CSC_LINK, MAC_CSC_KEY_PASSWORD,
CSC_NAME, APPLE_API_KEY, APPLE_API_KEY_ID, APPLE_API_ISSUER; Windows uses WIN_CSC_LINK
and WIN_CSC_KEY_PASSWORD. Stable packaging fails closed without signing credentials.
A blocked CI billing account means cross-platform execution remains unverified.

## Cloud release service

Apply Cloud migrations, build `cloud-api`, and restart only the owned test API. The
service uses its configured object storage and `HAARNESS_CLOUD_API_PUBLIC_URL` for
artifact URLs. Local tests can use the real S3 fixture; public deployment needs a
persistent S3/R2 bucket, HTTPS and working DNS. Operator mutations require the internal
token; normal account/API credentials must fail. Keep tokens out of arguments/logs.

```sh
# HAARNESS_RELEASE_TOKEN is injected securely from the local sandbox or release secret.
python3 scripts/release.py publish --index dist/releases/index-*.json \
  --url http://127.0.0.1:19080 --channel preview \
  --receipt dist/releases/receipt.json
```

The publisher streams each upload with length and SHA-256, atomically publishes the
complete artifact set, then downloads every public artifact and verifies hash/size.
Do not publish a partial platform matrix to stable. No npm or Docker publication is
part of this flow. Release URLs are public only after publication.

Public APIs: current `/api/v1/releases?channel=preview`, history
`/api/v1/releases/history?channel=preview`, immutable bytes `/api/v1/releases/artifacts/{id}`,
platform selection `/api/v1/releases/{channel}/{product}/download?os=macos&arch=arm64&kind=zip`.
The selector accepts `version` for a pinned historical version. Product manifests
preserve the entire original manifest, never a modified purported signature.

Operator APIs: POST `/internal/releases/artifacts` (raw bytes + metadata query),
POST `/internal/releases` (artifact IDs + version, sourceRevision, releasedAt),
POST `/internal/releases/{id}/promote` (explicit same-channel rollback), DELETE
`/internal/releases/{id}` (withdraw publication). Withdrawal doesn't silently select
an older release. An artifact stays public if another nonwithdrawn publication refers
to it; retain history/objects for rollback. Roll back by promoting a known good receipt ID.

## Install and execute the downloaded bytes

```sh
curl -fsSL http://127.0.0.1:19080/install.sh | \
  HAARNESS_RELEASE_URL=http://127.0.0.1:19080 HAARNESS_CHANNEL=preview \
  HAARNESS_INSTALL_DIR="$PWD/dist/releases/installed-cli" sh
./dist/releases/installed-cli/haarness --version
./dist/releases/installed-cli/haarness upgrade --channel preview \
  --release-url http://127.0.0.1:19080

curl -fsSL http://127.0.0.1:19080/install.sh | \
  HAARNESS_RELEASE_URL=http://127.0.0.1:19080 HAARNESS_CHANNEL=preview \
  HAARNESS_PRODUCT=desktop HAARNESS_INSTALL_DIR="$PWD/dist/releases/installed-desktop" sh
```

Default destinations: CLI `~/.local/bin`, macOS Desktop `~/Applications`, Linux
Desktop `~/.local/bin/Haarness.AppImage`, Windows CLI `%LOCALAPPDATA%\Haarness\bin`.
The canonical production command is `curl -fsSL https://app.haarness.dev/install.sh | sh`;
it is usable only after DNS, API deployment and stable publication are verified.
For Desktop add `HAARNESS_PRODUCT=desktop` to the `sh` environment. Windows uses
`irm https://app.haarness.dev/install.ps1 | iex` with equivalent environment variables.

The installer checks SHA-256 before replacing/running anything. On macOS it uses
`/usr/bin/xattr -dr com.apple.quarantine` on the verified staged executable/app,
including nested executables, matching Upstream's targeted cleanup. It leaves other
attributes and system security settings intact. Windows unblocks verified bytes.
Never clear quarantine on a downloaded file until its provenance/hash is verified.

```sh
python3 scripts/test-release-install.py
python3 scripts/test-release-quarantine.py  # macOS
HAARNESS_DESKTOP_EXECUTABLE="$PWD/dist/releases/installed-desktop/Haarness.app/Contents/MacOS/Haarness" \
  npm --prefix apps/desktop run test:e2e -- e2e/desktop.spec.ts \
  --grep 'real workspace: conversations, files, review and terminal'
```

Run native SDK server scenarios against the **installed CLI** (`HAARNESS_BIN=...`),
and an owned tmux echo-provider chat to prove the installed TUI. Launch the installed
Desktop, not the development Electron executable. A read-only downloaded DMG may carry
quarantine: test the installed copy after verified cleanup, not just a mounted bundle.
Use Chrome control to open `/download?channel=preview`, select platforms, copy the CLI
and Desktop commands, download both CLI and DMG, and compare their bytes with the receipt.
Verify unsupported platforms show no download and checksums remain available.

## Regression and SDK proof

Cloud: `cargo test -p cloud-contracts --test openapi_paths` and
`cargo test -p cloud-api --test releases` with a configured disposable test database.
Missing DB configuration skips integration tests and is not proof. Exercise concurrent
publishing, bad checksums, changed artifacts/versions, unpublished download denial,
invalid platforms, withdrawal and rollback. Check signed-manifest preservation when
reading a product. Re-export OpenAPI and regenerate both SDKs after route changes.

Run [SDK release tests](../haarness-sdk-surface/SKILL.md), native/Cloud typing, installer
fault tests, and actual packaged app tests. Check Chrome, API, Python sync/async and
TypeScript against the same service. Record hashes, version, source commit, API receipt,
platforms actually executed, quarantine proof and production/CI/signing limitations.
Clean up test keys, hosts, temporary channels and test processes; restore the prior
publication in failure paths. Never delete unrelated sessions, apps or build caches.
