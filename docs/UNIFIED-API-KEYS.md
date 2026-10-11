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

Verification: [2026-09-27 record](records/2026-09-27-unified-api-keys-verification.md).
