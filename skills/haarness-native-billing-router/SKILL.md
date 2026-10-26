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

The opt-in Electron test `haarness-account.spec.ts --grep 'unified API key'` uses `HAARNESS_ACCOUNT_E2E_URL` and `HAARNESS_API_KEY`. Run once with model invocation, billing and workspace capabilities, and once with only `models:read`. It verifies the restricted account dialog, catalog discovery, no OAuth file, and a real model reply for the combined key. Keep key expiry long enough for the suite and revoke only those test keys afterward.

`test-billing-tmux.py` and `test-cloud-tools-live.py` also accept `HAARNESS_API_KEY`. Cloud tools use a real orchestrator and remote model. Set `HAARNESS_CLOUD_TOOLS_OS` to the actual Cloud runtime OS when using a local provider; Linux is the default. When switching the sandbox from fixture inference to a real provider, configure the worker's `HAARNESS_CLOUD_DEFAULT_MODEL` to an available real model; `router/fixture/echo` is not in a real pricebook. Template launch must submit the requested chat message, not merely return a running workspace. Assert the eventual reply and clean every captured workspace/template ID even after a model assertion fails. A recovered invalid tool call is not success by itself; the expected remote operation and answer must complete.

## Explore beyond this guide

Treat the documented flows as a starting point. Do not hesitate to test relevant behavior that is not documented here: inspect the current implementation, follow unexpected results, and cover interactions and failure paths suggested by the task. Stay within the authorized environment and scope. Fix discovered bugs when authorized, verify the fixes, and record newly validated steps and remaining gaps so this skill improves with use.

## Multiple routers and your own provider keys

Every account has a default router. Named routers share the inference service but own their configuration and encrypted provider keys. Test management in Cloud → Routers and through all six generated SDK operations: list/create/get/update/delete and set/remove provider credentials. Mutations use optimistic revisions; stale writes must return 409. Default deletion is refused; foreign or deleted router IDs return 404. Incoming Haarness API keys need `models:read` for discovery, `models:invoke` for inference and `routers:write` for management; member roles still apply.

Use the `routerUrl` returned by the API. It includes `/routers/{id}/v1`; append `/models` or `/chat/completions` once. Legacy `/v1` uses the account default. Exercise `x-haarness-router-id` as well as the URL and reject conflicting selectors. On installations behind Caddy, verify the named path through the public proxy, not only the router's direct port. API and router require the same stable `HAARNESS_CLOUD_AGENT_SECRETS_KEY`; keys must never appear in responses, logs, screenshots, or SDK debug output.

In system mode, assert normal credit reservations and exact settlement. In BYOK mode, assert zero holds/debits, unchanged included and purchased balances, zero model pricing for Cloud charges, and usage quantities carrying `routerId` and `credentialMode`. Upstream providers may bill their own account. Test an empty wallet, missing/invalid/rotated/removed keys, model allowlists, provider errors, cancellation without usage, spool replay, and a mode change during a request. No path may fall back to system credentials when BYOK fails. Cloud compute is still billed separately.

For native proof, prepare an isolated login or API key and a funded default router plus a BYOK router with an allowed cheap model. `haarness account --routers` lists them; `haarness account --router UUID` persists selection. The TUI `/router` and Desktop account dialog must select the same account-owned configuration and refresh models without restarting the app. A removed selected router must fail explicitly. Check that account/issuer/key changes cannot inherit another identity's selection.

```sh
HAARNESS_BILLING_LIVE=1 HAARNESS_ROUTER_TEST_ID="$byok_router_id" python3 scripts/test-router-profiles-tmux.py
HAARNESS_ROUTER_TEST_ID="$byok_router_id" HAARNESS_ACCOUNT_E2E_URL="$cloud_api" HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- cloud-routers.spec.ts
```

The tmux script uses the Cloud API/config/web environment documented above and returns selection to the default. The Electron test requires a disposable `HAARNESS_API_KEY` with model and billing permissions and isolates local app state. Both make bounded real model calls. Inspect `target/router-profiles-tmux/` and Electron screenshots. Compare settled request IDs against billing usage; wallet equality alone can miss a settlement that has not arrived. For fixture integration, run Cloud `cargo test -p cloud-api -p cloud-router --test routers` with `HAARNESS_CLOUD_TEST_DATABASE_URL` set; absent DB configuration skips that proof.

### Provider configuration is independent of funded prices

Read `GET /api/v1/router/providers` (SDK `routerProviders()` / `router_providers()`) for the supported providers and their required settings. Compare the list in development and production configurations: missing operator keys or a missing/disabled pricebook must never hide customer-key setup. Use `providerSettings` in router responses to verify safe setting readback without exposing keys. Repeat save, replacement, removal and stale-revision rejection for every listed provider.

Bedrock uses bearer Bedrock API keys plus `settings.region`; it does not use the developer machine's AWS credentials. It discovers foundation models and paginated inference profiles through the Bedrock control-plane API. Azure uses `settings.resource`, its API key and model-list endpoint; a deployment name can differ from the catalogue model ID. Vertex uses `settings.project`, `settings.region` and a short-lived OAuth access token, and discovers Google's publisher models. Test token replacement and expiry. Do not claim real AWS/Azure/Vertex success from mocks when no account credentials were supplied.

Start with empty `settings.models` and verify that discovery populates the native and SDK model lists. The web form keeps additional model IDs under **Advanced model settings**; existing manual entries must remain editable and supplement discovery. Test pagination, duplicate IDs, non-chat model filtering, rejected credentials and missing list permissions. Bedrock profile-list denial must preserve successfully discovered foundation models; failed discovery must preserve explicitly configured models. Router's `models::discovery_tests` uses real local HTTP exchanges for these contracts without external credentials. Cloud's unpriced-model integration test uses a locally rejected header to verify fallback without contacting AWS.

With a customer-key router, test a model absent from the system pricebook, no operator credential for that provider, an empty Cloud wallet, direct model calls and `/models` discovery. Assert zero reservations and settled charges, preserved managed pricebook, key/account/router allowlists, and no fallback after credential removal. The all-provider API lifecycle and unpriced Bedrock accounting tests are in Cloud's `routers` suites; real AWS event framing, tool calls and malformed-frame rejection are in Router's provider tests. Keep fixtures and live-account evidence distinct.

For bounded live inference, Cloud's `scripts/test-router-providers-live.py` uses the generated Python SDK to create its own router and API key, discover models, run JSON and streaming requests, replace/remove credentials, and verify settled zero-credit usage. Run with the SDK's Python virtual environment, `HAARNESS_PROVIDER_E2E=1`, the loopback `HAARNESS_CLOUD_API_URL`, a current private OAuth JSON file in `HAARNESS_E2E_OAUTH_FILE`, and a private provider environment file in `HAARNESS_PROVIDER_ENV_FILE`. That environment file supplies `HAARNESS_CLOUD_ROUTER_PROVIDERS` and the referenced secrets. `HAARNESS_PROVIDER_E2E_PROVIDERS` selects a comma-separated subset of `openai,anthropic,gemini`. Refresh expired OAuth grants privately before running. Do not treat an upstream outage or lack of credentials as live-provider success, or put credentials into command arguments or captured evidence.
