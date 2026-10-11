# Multiple Cloud routers verification — 2026-09-28

Implementation bases: native `ab4e494e182ae544f025cb1f9e5ca74d5b5d836a`,
Cloud `2f82f593f6385835302a7dc8032128ad0a479335`, standalone router `9d4c2c4`.

## Behavior

Each account receives a protected default router. Additional named routers are
configurations on the shared inference service, with account authorization,
optimistic revisions, model allowlists and encrypted write-only provider keys.
System mode retains normal credit admission and settlement. BYOK mode has no
Cloud model debit or credit hold, records usage, and never falls back to system
credentials. Compute billing remains separate. Requests pin their credential and
billing mode before invoking the provider. Provider errors using a customer's key
cannot trip or heal the shared system-key circuit breaker.

Cloud has a Routers management page with account WebSocket refreshes that preserve
open forms. The TypeScript and Python sync/async SDKs expose all six management
operations. CLI, TUI and Desktop persist selection per Cloud issuer and account
(or explicit API-key identity), refresh models, and fail explicitly when a saved
router is invalid or deleted. Credentials remain outside the renderer.

## Automated validation

| Surface | Evidence |
| --- | --- |
| Standalone router | Core, provider and server suites: 52 tests, including 15 HTTP end-to-end cases; repeated customer failures leave the system circuit usable |
| Cloud API | Full API suite passed; final router lifecycle test repeated after required-nullable credential input hardening |
| Cloud router | Full suite passed with four test threads; final selector unit test and two database router cases repeated after selector changes |
| Credit accounting | System exact debit; BYOK JSON/SSE zero debit/holds, empty wallet, cancellation, replay, request-pinned key/mode, rotation/removal, permissions, foreign/deleted routers and allowlists |
| Encryption | Ciphertext round-trip, account/router/provider binding, tamper rejection, missing master key and no plaintext storage |
| Cloud web | 155 tests passed; final Routers/i18n subset passed; typecheck, production build and public-page prerender passed |
| Native | Account 42, provider 94 and TUI 356 tests passed; CLI built |
| Desktop | 148 unit tests passed; build/typecheck and opt-in real Electron router journey passed |
| SDKs | TypeScript 15 and Python 26 tests passed; generated-contract check and TypeScript build passed |
| Migration and docs | Forward migration policy and dependency checks passed; seven testing skills validated |

The small shared PostgreSQL fixture exhausted its lock table during the first
unbounded parallel router run. The complete suite passed with four test threads,
without relaxing assertions or timeouts. The OIDC integration initially pointed
at the old fixture port and passed after correcting the test environment. The
separate live S3/R2 test remains intentionally skipped without a configured bucket.

## Live proof

Used the isolated local Cloud stack and fixture account, real OpenAI
`openai/gpt-4.1-nano`, generated SDK clients, Chrome control, actual tmux TUI and
Electron. Test account state and credentials stayed in ignored private files.

- Chrome: create/configure BYOK router, save/remove a fixture provider key,
  inspect revision updates without losing the mounted form, and delete the
  disposable router. Usage shows BYOK rows as “Your provider key”.
- Python and TypeScript SDKs: real management lifecycle, provider configuration,
  discovery and cleanup. Real inference used the returned named router endpoint.
- Direct system request `req_01a0e5ff-0f43-7438-b14e-4239ea3d3cc7` settled at
  3,200 nanos. BYOK JSON `req_01a0e5ff-1695-7540-87d8-32c2a1b5fbb0` and SSE
  `req_01a0e5ff-1966-70ca-8052-f4a89dabc28f` settled at zero with unchanged wallets.
- Actual Caddy proxy: named model discovery and streamed reply passed through
  `/routers/{id}/v1`. Request `req_01a0e607-47c8-7129-ad39-93e1b27b4974` returned
  `PROXY_ROUTER_OK` and settled at zero. This caught and fixed a proxy matcher that
  accepted model discovery but missed the deeper chat-completions path.
- TUI: `/router` selects BYOK and default; real replies in both modes; only system
  mode debits credits; zero settled holds; 100- and 80-column captures checked.
- Desktop: account selector, refreshed model catalog, zero BYOK model prices,
  real replies in both modes and matching wallet behavior; isolated app state.

The live system and BYOK calls used the same available upstream test credential;
this proves the request path and Cloud billing policy, not separate provider
invoices. Deterministic tests prove distinct customer credentials are forwarded.
Live third-party validation was OpenAI only; other adapters were covered by their
automated suites. This was local validation, not a production deployment.

## Rollout and repeatability

Apply Cloud migration `0517_routers.sql` and deploy compatible API/router/web
builds. API and router must share the stable `HAARNESS_CLOUD_AGENT_SECRETS_KEY`.
Retain that encryption key across restarts. Both Caddy configurations must route
`/routers/*` to cloud-router. Old `/v1` clients use the account default.

See `skills/haarness-native-billing-router/SKILL.md` for repeatable API, SDK,
Chrome, tmux and Electron scenarios, including negative paths and accounting
assertions. Every test skill now explicitly encourages relevant undocumented
testing, investigation of unexpected results, and recording validated discoveries.
