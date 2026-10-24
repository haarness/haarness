# API keys across Haarness clients

How one Haarness Cloud API key authorizes the CLI and TUI, Desktop and the SDKs, for Cloud operations
and for model inference alike, and how it ranks against a saved sign-in. For developers and operators
running Haarness unattended (CI, scripts, servers) and for contributors working on account code.

> **User guide:** [Automate with the CLI and SDKs](https://haarness.ai/docs/cloud/cli-and-sdk).

Set `HAARNESS_API_KEY` for the CLI/TUI, the Desktop process or an SDK client. A
key starts with `hck_` (older `hcr_` router keys are still accepted); anything
else is refused before a request is made. The CLI, TUI and Desktop sign in to
`https://haarness.ai` in a release build and to the local Cloud stack
(`http://127.0.0.1:18080`) in a development build; set `HAARNESS_CLOUD_API_URL`
only to use another Cloud. The SDKs read the key from the environment but take
the Cloud URL as their `baseUrl`.

A key can authorize Cloud operations and managed model inference; the same
secret is used for both. Configure the capabilities and expiry in Cloud's single
**API keys** page. API keys bind to one account: `haarness account` cannot
switch to another while one is set. Explicit SDK tokens take precedence; an
environment API key takes precedence over saved native OAuth or the older SDK
`HAARNESS_CLOUD_TOKEN`. Invalid, expired or revoked keys never fall back to the
saved login (the CLI says "API key invalid, expired or revoked. Update
HAARNESS_API_KEY."). Keys cannot modify payments, membership or other keys.

Keys carry model allowlists, direct model spending caps, rate and concurrency
limits. Account limits also apply. Cloud runtime/scheduled agent work uses
account/runtime billing rather than the invoking key's direct inference cap.
Revocation blocks subsequent requests; it does not cancel already accepted work.
Unsetting the environment variable disconnects the key without revoking a saved
browser login; `haarness logout` refuses while the key is set and says so. To
disable all uses, revoke the key in Cloud.

Native account status (`haarness account --json`, and the account bridge
Desktop uses) reports `authKind: "api_key"`, its account and scopes, without
exposing the secret. Desktop hides unavailable billing/session actions. Its
messages are translated in every supported interface catalog. Create new keys
from the unified screen or API.

Verification: [2026-09-27 record](records/2026-09-27-unified-api-keys-verification.md).
