# Contributing to Haarness

Thanks for helping. This file explains how to get a change merged.

By participating you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
Security problems go to [SECURITY.md](SECURITY.md), never to a public issue.

## Before you start

- **Bugs**: open an issue with the bug template first, unless the fix is small and obvious.
- **Features and larger changes**: open an issue describing the problem before writing
  code, so we can agree on the approach. Unsolicited large pull requests are hard to review
  and may be declined.
- Small documentation fixes and typo corrections can go straight to a pull request.

## Development setup

You need the toolchain pinned in `rust-toolchain.toml` (installed by rustup on first use).
Desktop work additionally needs Node.js 22+ and npm; some packaging steps need Bun (see
[Desktop](docs/DESKTOP.md)). Normal Rust builds are offline: OpenTUI and ripgrep are vendored.

```sh
cargo build -p haarness-cli
./target/debug/haarness doctor
```

The shortest loop for each surface (CLI/TUI/server, SDKs, Desktop) is described in
[`.skills/haarness-build-run/SKILL.md`](.skills/haarness-build-run/SKILL.md). Please keep
the `target/` cache warm rather than cleaning it between builds.

## Checks

CI runs these on Linux, macOS and Windows; run the ones your change touches before pushing:

```sh
cargo fmt --all -- --check
cargo run -p xtask -- architecture          # crate dependency rules
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                      # or: cargo test -p <crate>
sh ./scripts/check-commercial-boundary.sh
```

Also, depending on the change:

- **Server API or protocol**: regenerate and test the SDKs — `./scripts/test-sdks.sh`.
- **Desktop**: `npm --prefix apps/desktop run typecheck`, `npm --prefix apps/desktop test`,
  and the relevant Playwright flow with `npm --prefix apps/desktop run test:e2e -- --grep '…'`.
- **TUI**: exercise the change in a real terminal; the `scripts/test-*-tmux.*` scripts show
  how the existing flows are driven.
- **User-visible text**: add matching keys to every locale file in `locales/` (see
  [`locales/README.md`](locales/README.md)); Desktop must stay correct right-to-left in Arabic.

## Pull requests

- Keep each pull request to one logical change, with tests for new behaviour and for the bug
  being fixed.
- Write commit messages in the imperative (“Add …”, “Fix …”) and explain *why* in the body.
- Update the documentation in `docs/` and `README.md` when behaviour changes. Write what is
  true of the shipped build.
- Do not commit secrets, credentials, personal data or generated build output.

## Licensing of contributions

Haarness is licensed under the [Apache License 2.0](LICENSE). Unless you state otherwise,
any contribution you intentionally submit for inclusion is licensed under the same terms,
without any additional terms or conditions (section 5 of the license).

Sign off each commit to certify the [Developer Certificate of Origin](https://developercertificate.org/):

```sh
git commit -s -m "Fix …"
```

This adds a `Signed-off-by: Your Name <you@example.com>` line stating that you wrote the
change or otherwise have the right to submit it under the project's license.
