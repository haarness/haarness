# API keys across Haarness clients

Set `HAARNESS_API_KEY` and `HAARNESS_CLOUD_API_URL` for the CLI/TUI, Desktop process
or SDK client. A key can authorize Cloud operations and managed model inference;
the same secret is used for both. Configure the capabilities and expiry in Cloud's
single **API keys** page. API keys bind to one account. Explicit SDK tokens take
precedence; an environment API key takes precedence over saved native OAuth or
the older SDK `HAARNESS_CLOUD_TOKEN`. Invalid, expired or revoked keys never fall
back to the saved login. Keys cannot modify payments, membership or other keys.

Keys carry model allowlists, direct model spending caps, rate and concurrency
limits. Account limits also apply. Cloud runtime/scheduled agent work uses
account/runtime billing rather than the invoking key's direct inference cap.
Revocation blocks subsequent requests; it does not cancel already accepted work.
Unsetting the environment variable disconnects the key without revoking a saved
browser login. To disable all uses, revoke the key in Cloud.

Native account status exposes `authKind: "api_key"`, its account and capabilities,
without exposing the secret. Desktop hides unavailable billing/session actions.
Its messages are translated in every supported interface catalog. Legacy `hcr_`
keys remain accepted; create new keys from the unified screen/API.

## Verification

This change was verified against native base `4443693744b6e0d1ea702c2f62915d240fe6c42e`
and Cloud base `149c03065281a9994713d9b3fd7c630648438960`, with real OpenAI calls
through the local managed router. See Cloud's
`docs/UNIFIED-API-KEYS-VERIFICATION-2026-09-27.md` for cross-repository coverage.
Native tests include 135 account/provider tests, 148 Desktop unit tests, two live
Electron capability cases, real tmux billing and LLM-driven Cloud tools, and
14 TypeScript / 24 Python SDK tests plus the live cross-SDK journey.

Reusable instructions are in [SDK surface](../skills/haarness-sdk-surface/SKILL.md)
and [native billing/router](../skills/haarness-native-billing-router/SKILL.md).
The new `sdks/tests/api_keys_e2e.py` includes the TypeScript companion and cleans
its keys/session; the existing tmux/Cloud tool scripts accept a private
environment key. Generated OpenAPI and SDK changes must be checked against the
intended Cloud checkout via `HAARNESS_CLOUD_REPO`.
