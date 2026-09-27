---
name: haarness-sdk-surface
description: Verify Haarness Python sync/async and TypeScript SDK coverage of runtime and Cloud APIs, including billing, sessions/chats, schedules, files, marketplace and WebSocket events.
---

# SDK contract and end-to-end coverage

The native repository contains `sdks/`; Cloud contracts generate `haarness-cloud/docs/openapi.json`. Read `haarness/docs/CLOUD-SDK-VERIFICATION-2026-09-27.md` and the native build skill. Regenerate from the matching Cloud checkout, not a stale sibling primary checkout:

```sh
# Cloud checkout
scripts/openapi.sh --update
# Native checkout
cp "$cloud_repo/docs/openapi.json" sdks/cloud-openapi.json
HAARNESS_CLOUD_REPO="$cloud_repo" python3 sdks/generate.py
HAARNESS_CLOUD_REPO="$cloud_repo" python3 sdks/generate.py --check
npm --prefix sdks/typescript run build
npm --prefix sdks/typescript test
sdks/python/.venv/bin/python -m pytest sdks/python/tests -q
```

Use the smallest relevant tests during iteration. `scripts/test-sdks.sh` is the full packaging/fresh-consumer gate; inspect it first because it can install dependencies. Do not reinstall node_modules while another test uses them.

The SDK must expose every documented Cloud HTTP operation and WebSocket stream, and every server runtime operation. Test Python sync and async and TypeScript with real account auth/refresh. Check method/path/query/body encoding, binary uploads/downloads, errors, cancellation, pagination and redirect policy. Request options must remain compatible when adding account-scoped queries.

Run `sdks/tests/cloud_e2e.py` and `cloud_ts_e2e.mjs` against a disposable Cloud fixture stack, and native server lifecycle/file tests. Read their flags and `cloud_fixture.py` authentication helper. Explicit consent may be required when fixture auto-consent is disabled; do not bypass OAuth on a real account.

Cover independent session launch, agent-template launch, multiple chats, shared files, turn streams, Cloud schedules, marketplace installation and cleanup. Billing checks include accountId on reads and mutations, wallet/holds, plans, checkout, portal, cancel/resume, payments, usage and ledger. Verify account membership and least-privileged tokens; a billing-only event token sees billing changes and no session data.

For live streams, test ready/resync, correct account filtering, updates during a snapshot fetch, disconnect/reconnect, refreshed credentials and stopping the iterator. Assert state converges from authoritative HTTP snapshots without duplicate monetary effects.

Capture exact source SHAs, generated-contract checks, test results, real provider/fixture distinction and cleanup outcomes. Existing verification reports are references, not current pass results.
