# Unified API keys verification — 2026-09-27

Record of the checks behind [API keys across Haarness clients](../UNIFIED-API-KEYS.md).

This change was verified against native base `4443693744b6e0d1ea702c2f62915d240fe6c42e`
and Cloud base `149c03065281a9994713d9b3fd7c630648438960`, with real OpenAI calls
through the local managed router. See Cloud's
`docs/UNIFIED-API-KEYS-VERIFICATION-2026-09-27.md` for cross-repository coverage.
Native tests include 135 account/provider tests, 148 Desktop unit tests, two live
Electron capability cases, real tmux billing and LLM-driven Cloud tools, and
14 TypeScript / 24 Python SDK tests plus the live cross-SDK journey.

Reusable instructions are in [SDK surface](../../skills/haarness-sdk-surface/SKILL.md)
and [native billing/router](../../skills/haarness-native-billing-router/SKILL.md).
The new `sdks/tests/api_keys_e2e.py` includes the TypeScript companion and cleans
its keys/session; the existing tmux/Cloud tool scripts accept a private
environment key. Generated OpenAPI and SDK changes must be checked against the
intended Cloud checkout via `HAARNESS_CLOUD_REPO`.
