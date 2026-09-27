---
name: haarness-native-billing-router
description: Test Cloud router metering, native TUI and Desktop billing balances, model prices, account changes and live billing updates with a real Haarness model call.
---

# Native billing and router proof

Read `haarness/.skills/haarness-build-run/SKILL.md`. Use an isolated `HAARNESS_CLOUD_CONFIG_DIR`, `HAARNESS_DATA_DIR`, Desktop data directory and disposable project. Obtain tokens through the fixture OAuth flow or normal login. Never replace the user's credentials. Fund the test account through a sandbox payment; distinguish fixture grants from real checkout proof.

## Automated and real-client checks

From `haarness/`:

```sh
cargo test -p haarness-account
cargo test -p haarness-providers cloud_catalog_prices_override_builtin_estimates
cargo test -p haarness-tui --lib
cargo build -p haarness-cli
npm --prefix apps/desktop test
npm --prefix apps/desktop run build
```

Run the real TUI journey (one small billable call) using an already authorized isolated account:

```sh
HAARNESS_BILLING_LIVE=1 HAARNESS_CLOUD_WEB_ORIGIN="$cloud_web" HAARNESS_CLOUD_API_URL="$cloud_api" HAARNESS_CLOUD_CONFIG_DIR="$test_account_dir" python3 scripts/test-billing-tmux.py
```

It checks the footer, `/billing`, a real model reply, included-credit debit, purchased credits remaining unchanged, released holds, live balance delivery, and 100/80-column panels. Inspect `target/billing-tmux/` captures. Do not use only a handler unit test as TUI proof. Also try `/billing open`, `/billing topup`, `/billing usage`, sign-out, account switch and offline/reconnect. Stale balances must be identified and previous-account balances cleared.

Run the opt-in Electron browser-login/model/billing journey:

```sh
HAARNESS_BIN="$PWD/target/debug/haarness" HAARNESS_ACCOUNT_E2E_URL="$cloud_api" HAARNESS_CLOUD_WEB_ORIGIN="$cloud_web" HAARNESS_ACCOUNT_E2E_BILLING=1 npm --prefix apps/desktop run test:e2e -- haarness-account.spec.ts
```

Requires fixture OIDC and a funded account with `openai/gpt-4.1-nano` available. Inspect `billing-live.png` and confirm plan, included/purchased/reserved amounts, renewal/reset, overage and billing actions. Account credentials stay in main/native code, never the renderer. Verify balances update without dialog remount or lost input. Billing links must preserve the selected account.

## Router accounting

Discover available models from the actual router URL returned by the key API; it already includes `/v1`. Test JSON and streaming chat completions, including SSE token usage. Reassemble streaming text before assertions. Use cheap bounded calls. Record request IDs and tariff/pricebook version.

Wait for each request's settlement record and zero matching reserves, not merely a decreased wallet balance. Check `granted - consumed - reserved = available`, request charge = token quantities × pinned tariff with the server's rounding rule, wallet debit = settlement debit, and a balanced ledger. Compare nanos/integers, never floating point for accounting. Concurrent requests require per-request attribution rather than attributing the whole account delta to one call.

Test included-first funding, purchased fallback with overage enabled, denial with overage disabled/insufficient balance, provider failure, and cancellation/reservation release. Fixture requests can verify edge cases deterministically; real provider calls prove adapter integration. Native displayed model pricing must use Cloud's published per-token prices rather than built-in estimates. A subscription purchase and router consumption are separate proofs.

Revoke only keys created by the test, stop owned daemons/tmux sessions, and retain redacted evidence. See `haarness-router/docs/testing.md` for provider suites and `haarness-cloud/scripts/e2e.sh` for the account/ledger fixture journey.

## API-key authentication

Repeat relevant native tests with a disposable `HAARNESS_API_KEY` and no credentials file. Pass the key privately via the subprocess environment; do not put it in tmux command arguments or logs. The account adapter and Cloud provider must use that same key, and must not create an OAuth grant or child inference key.

The opt-in Electron test `haarness-account.spec.ts --grep 'unified API key'` uses `HAARNESS_ACCOUNT_E2E_URL` and `HAARNESS_API_KEY`. Run once with model invocation, billing and session capabilities, and once with only `models:read`. It verifies the restricted account dialog, catalog discovery, no OAuth file, and a real model reply for the combined key. Keep key expiry long enough for the suite and revoke only those test keys afterward.

`test-billing-tmux.py` and `test-cloud-tools-live.py` also accept `HAARNESS_API_KEY`. Cloud tools use a real orchestrator and remote model. Set `HAARNESS_CLOUD_TOOLS_OS` to the actual Cloud runtime OS when using a local provider; Linux is the default. When switching the sandbox from fixture inference to a real provider, configure the worker's `HAARNESS_CLOUD_DEFAULT_MODEL` to an available real model; `router/fixture/echo` is not in a real pricebook. Template launch must submit the requested chat message, not merely return a running session. Assert the eventual reply and clean every captured session/template ID even after a model assertion fails. A recovered invalid tool call is not success by itself; the expected remote operation and answer must complete.
