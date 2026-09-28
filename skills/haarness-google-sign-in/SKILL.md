---
name: haarness-google-sign-in
description: Configure and test real Google sign-in for Haarness Cloud development and production, including browser sessions and native CLI/Desktop authentication.
---

# Haarness Google sign-in

Read the matching Cloud checkout's `docs/DEVELOPMENT.md` and native
`.skills/haarness-build-run/SKILL.md`. The parent workspace can contain multiple Cloud
checkouts; identify the running source, ports and database before changing configuration.

## Configuration

Use separate Google projects and **Web application** OAuth clients for development and
production. The Google callback belongs to the Cloud API, not the frontend dev server:

| Stack | Registered callback |
| --- | --- |
| Native development API | `http://127.0.0.1:18080/auth/google/callback` |
| Docker development proxy | `http://localhost:18000/auth/google/callback` |
| Separate Stripe sandbox stack | `http://127.0.0.1:19080/auth/google/callback` |
| Production | `https://app.haarness.ai/auth/google/callback` |

Verify those ports against the active configuration. Keep local callbacks out of the
production client. External audience supports commercial customers outside the company's
Workspace; keep development in Testing and add the intended test users. Request only
`openid email profile`; Gmail and Drive access are unnecessary.

The API reads `HAARNESS_CLOUD_OIDC_ISSUER=https://accounts.google.com`,
`HAARNESS_CLOUD_OIDC_CLIENT_ID`, and `HAARNESS_CLOUD_OIDC_CLIENT_SECRET`. Store secrets
in ignored mode-0600 files or the deployment secret store, separately by environment.
Do not expose them in screenshots, logs, commits, frontend variables or tool output.

Docker preparation reads the distinct `HAARNESS_CLOUD_DEV_OIDC_CLIENT_ID` and
`HAARNESS_CLOUD_DEV_OIDC_CLIENT_SECRET` from `.env`; without them it intentionally uses
the fixture identity provider. A plain `dev-up.sh` rerun leaves a live API alone: restart
the owned API process when changing credentials. Preserve Stripe/R2/provider settings.

## Verification

- Check `/readyz` reports OIDC discovery healthy and `/auth/options` enables Google.
  Inspect the start redirect without logging the full URL: Google host, correct client
  and callback, exact identity scopes, S256 PKCE, state and nonce.
- In Chrome, sign out of **Haarness**, follow Continue with Google, choose the intended
  account, and verify return to the configured web origin with the correct profile.
  Sign out and repeat; confirm the same Haarness user/account is reused. Test cancellation
  and a protected-page return path. Capture proof without tokens or callback query strings.
- Run Cloud `identity_flow` integration tests with a real test database and the matching
  `cloud-fixtures` binary. They cover state/cookie binding, replay, tampering, tenant
  isolation, native PKCE, refresh rotation/reuse and device approval. They use a fixture
  issuer and are **not** evidence of a working Google console registration.
- For native proof, isolate `HAARNESS_CLOUD_CONFIG_DIR`, set `HAARNESS_CLOUD_API_URL`,
  and use `haarness login --no-browser --json` (also `--device` for headless login).
  Open the emitted URL in Chrome, approve the expected Haarness client, and verify
  `haarness account --json`. CLI/Desktop receive Haarness tokens, never Google secrets.
  Revoke only the test grant afterward; preserve the user's existing login.
- Check cookie/CSRF behavior when frontend and API ports differ. Use one loopback
  hostname consistently; `localhost` and `127.0.0.1` do not share cookies.

Keep the requested development services running for the user. Report fixture versus live
proof separately, and do not claim production works until its DNS, HTTPS callback,
deployment secrets, consent publication and actual login have been verified. Test useful
undocumented scenarios discovered during the work; this skill is not a coverage ceiling.
