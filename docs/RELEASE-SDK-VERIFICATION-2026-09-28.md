# Binary release and SDK verification — 2026-09-28

Native release source: `540a1127a42bcb2dffcd1f563b8fbb05e8069140`, clean tracked checkout, tag `v0.1.1-preview.1`.
Cloud API source: `dddac4b37d10d6f5db70969d9363dfc3514b07c8`.
Both implementations are pushed to their repositories' `main` branches.
The documentation commit containing this report follows the native release source commit.

## Delivered behavior

- Cloud operator upload, immutable publication, current/history, platform/version selection,
  public binary downloads, explicit rollback and withdrawal. Streaming upload length/SHA-256
  checks and atomic channel publication prevent inconsistent releases. Unpublished and
  withdrawn-only artifacts are unavailable publicly.
- A product selector preserves the full published manifest, including any existing signing
  metadata. This release uses HTTPS transport and SHA-256; it does not claim a cryptographic
  manifest signature.
- Shell and PowerShell installers, CLI `haarness upgrade`, native artifact collection and
  publication tooling, and a six-platform CI workflow. No npm or Docker publication.
- macOS curl installation recursively removes `com.apple.quarantine` from the **verified
  staged** CLI/app using `/usr/bin/xattr`, including nested executables. Other attributes
  and global system protections remain unchanged. Windows unblocks verified downloads.
- Download page selects stable/preview and platform, exposes CLI and Desktop commands and
  SHA-256 values, and reports missing releases/platforms honestly.
- SDK inventory now covers 123 native operations and 156 Cloud operations, with manual
  WebSocket/SSE/runtime adapters. Added browser host/draft sockets, TCP tunnel/adoption,
  shared HTML, Cloud connector sockets, release binary/text APIs, and explicit-key router
  inference for default and named routers. New routes must pass source-driven coverage checks.

## Real local publication

Release version: `0.1.1-preview.1`. Channel: `preview`.
Publication ID: `01a0e6d0-ba2e-7522-b849-d3bd0c84fb21`.
API: `http://127.0.0.1:19080`; download page: `http://127.0.0.1:19173/download?channel=preview`.
The existing local fixture stack uses real Postgres and an S3-compatible fixture.
Every payload was uploaded through the operator API and downloaded from the public API;
its full byte count and SHA-256 matched the build index.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `haarness-macos-arm64` | 41039104 | `cf165e879a3ce3b49cf632259b098b7568aa554cdda9a43c9ab232e7d7aa19b3` |
| `Haarness-0.1.1-preview.1-mac-arm64.dmg` | 207335309 | `58190298b4605faa951a35317f0d076d24caa584b57cbfc59bc54c758fecc82d` |
| `Haarness-0.1.1-preview.1-mac-arm64.zip` | 207373843 | `25bc6fe25a9fe8d6bb9b880bc62d376a077e93a35cd474f569cd6ddbe23604f0` |

The CLI was installed pinned to 0.1.0 and upgraded to this preview using the served
installer. The current standalone binary matches the index above. Desktop was installed
from the served ZIP; its bundled backend matches the packaged backend and prints the new
version. Its backend's digest differs from the standalone CLI because app packaging signs
that executable. Quarantine is absent from the installed app, standalone CLI and nested backend.

Installed fixtures retained for inspection:

- `dist/releases/installed-cli/haarness`
- `dist/releases/installed-desktop/Haarness.app`
- `dist/releases/index-macos-arm64.json`
- `dist/releases/receipt-preview-0.1.1.json`

## Passed validation

- CLI crate: 117 unit tests and 7 integration tests (ACP, hot reload, insights,
  telemetry and profiles). Desktop: 148 unit tests.
- Cloud web: 155 tests, TypeScript checking, production build and static prerender.
  Updated install documentation also passed 5 prerender tests.
- Cloud API: 8 smoke tests, the complete release lifecycle regression, 12 marketplace tests,
  3 OpenAPI coverage tests, strict `cargo clippy -p cloud-api --all-targets -- -D warnings`,
  and OpenAPI export drift check. Integration tests used a configured test database.
- Release lifecycle covers simultaneous initial publication, idempotency, checksum failure,
  immutable payload/version rejection, unpublished denial, exact bytes, product integrity,
  historical version selection, rollback, withdrawal and unsupported platform.
- Installer fault tests preserve the existing executable on corrupt/missing checksums and
  remove staging. macOS quarantine regression injects quarantine onto extracted app and
  nested executable, then proves removal and execution.
- SDK: 20 TypeScript tests, 30 Python tests, TypeScript build and Python mypy; generated
  artifacts match the current Cloud checkout. Source inventories cover every registered route.
- Downloaded and installed CLI: **119/119 non-account native operations in each of
  TypeScript, Python sync and Python async**, including real browser-host sockets, tunnel
  frames, draft adoption, shared HTML, chat/turns, PTYs, files and cleanup.
- Downloaded and installed CLI in tmux: chat, streaming, selectors, nested terminal,
  resize, suspend/resume and exit; cursor state restored correctly.
- Downloaded and installed Desktop executable: real Electron end-to-end workflow for
  conversations, file editing/saving, review/comments and terminal, using its own bundled backend.
- Live Cloud SDK tests in all three clients: release upload/publication/history/download/
  withdrawal/rollback; connector ready/send/connected readback; real OpenAI model discovery,
  JSON inference and SSE through the default and named BYOK routers. Test keys and hosts revoked.
- Chrome showed the final version and checksums, stable empty state, and copied the exact
  Desktop curl command. Earlier Chrome CLI/DMG downloads of 0.1.0 matched hashes. The final
  version's repeat Chrome download was interrupted when browser control became unavailable;
  final artifact downloads were fully verified through the API and curl instead.
- Native and Cloud commercial-boundary checks passed. New release skill validated.

## Boundaries and external blockers

This is complete **local macOS ARM64 release proof**, not a production or six-platform pass.

- The initial release validation used an incorrect public hostname. The canonical domain
  is `haarness.ai` (Cloud API: `app.haarness.ai`); public deployment and publication were
  not verified by this local release run.
- Native [release workflow](https://github.com/orex-haarness/haarness/actions/runs/36393744069)
  and [Cloud CI](https://github.com/orex-haarness/haarness-cloud/actions/runs/36388959499)
  failed before executing steps: GitHub reports failed account payments or a spending limit.
  Windows, Linux and Intel Mac artifact execution is therefore unverified.
- The macOS preview is ad-hoc signed, not notarized. Curl installation handles quarantine;
  browser-downloaded unsigned DMGs can still trigger Gatekeeper. Public signed distribution
  needs the commercial signing/notarization credentials described in the release skill.
- Desktop's optional existing auto-update feed was not configured or tested. This work
  delivers installers and manual reinstall/CLI upgrade, not an Electron auto-updater service.
- Four native account adapters and the broader Cloud billing/session/schedule workflows were
  not all rerun in this release audit; their previous verification reports remain separate.
  Native SDK browser protocol tests use a fixture host; installed Electron proof is separate.
- No standalone Router service binary distribution, npm packages, Docker images or production
  infrastructure were published in this change.
- The owned mounted test DMG was ejected; fixture servers remain running for manual testing.
  Unrelated dirty primary Cloud work and its dependency symlink were preserved.

Follow [release testing](../skills/haarness-release/SKILL.md) and
[SDK testing](../skills/haarness-sdk-surface/SKILL.md). Explore relevant undocumented cases
and keep actual evidence separate from prerequisites or previous results.
