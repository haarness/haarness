---
name: haarness-payments-ngrok
description: Configure and test Haarness Cloud Stripe sandbox payments through ngrok, including checkout, trials, automatic top-ups, yearly and per-seat Team plans, portal, refunds, subscription changes, webhook recovery and ledger proof.
---

# Stripe and ngrok payment verification

Run from the Haarness workspace root, which contains `haarness/`, `haarness-cloud/`, and `haarness-router/`. Use a clean checkout or worktree for source changes; preserve any existing Cloud UI work. Read the native `.skills/haarness-build-run/SKILL.md` before rebuilding native binaries.

Stripe delivers webhooks to a public HTTPS URL, so a local stack only receives them through the ngrok tunnel below: no tunnel, no grants. Checkout's success redirect goes back to the local web origin in the browser and needs no tunnel, but it never grants anything on its own.

Use an isolated Stripe sandbox, local database, ports and fixture user. Never run payment fixtures against live Stripe keys or a production database. Test cards are `4242424242424242` (success), `4000000000009995` (insufficient funds), `4000002500003155` (3DS) and `4000000000000341` (saves fine, every later off-session charge is declined: the automatic top-up decline). Use a future expiry and test CVC. In API tests prefer Stripe's documented test PaymentMethod IDs. Confirm the current Stripe account ID with the API before setup. Keep keys, signing secrets, OAuth tokens and ngrok tokens in ignored files with mode 0600; do not capture them in screenshots or logs.

## Configure the complete path

1. In Chrome, select/create the intended Stripe sandbox. Products and prices must match `haarness-cloud/config/plans.v1.json`; rerun the helper after every catalog change. It reuses matching products and prices, creates missing ones (monthly and yearly plans, Team with an adjustable quantity, every top-up pack), rewrites product descriptions from the catalog (Checkout and invoices show them, so a stale one advertises old allowances) and writes `prices.json`. Run the following commands from the selected Cloud checkout (or worktree). Use the repeatable setup helper:

   ```sh
   python3 scripts/stripe-sandbox.py --help
   python3 scripts/stripe-sandbox.py --key-file .local/stripe/secret-key --account "$stripe_sandbox_account" --webhook-url "$stripe_webhook_url" --output .local/stripe
   ```

2. Pin the Stripe webhook wire version to `2025-08-27.basil`. The setup script configures the required events (including `payment_intent.succeeded`/`payment_intent.payment_failed` for automatic top-ups), products, top-ups and customer portal. Upgrades and added seats invoice immediately with proration; downgrades and fewer seats take effect at period end. Load the generated price bindings (every `HAARNESS_CLOUD_STRIPE_PRICE_*` in `prices.json`), secret key and signing secret into the isolated API/worker process environment, never into the frontend bundle, and without printing them. The API logs "Stripe prices missing" at startup for any catalog product without a binding. For the existing stack, update its `stack.env`, then restart it with `HAARNESS_CLOUD_RUN_DIR=<run dir> scripts/dev-down.sh` and `HAARNESS_CLOUD_ENV_FILE=<run dir>/stack.env HAARNESS_CLOUD_RUN_DIR=<run dir> HAARNESS_CLOUD_SKIP_BUILD=1 scripts/dev-up.sh` after `cargo build --workspace`. Set `HAARNESS_CLOUD_BILLING_SWEEP_INTERVAL_SECS=10` there so automatic top-ups, alerts and yearly months run within seconds. Never edit a migration the stack's database already applied; add the next one.
3. Run `ngrok version`, then `ngrok update` if needed. Preserve the user's global ngrok configuration and existing tunnels. Use a private dedicated config for the selected dashboard account/domain. Do not change the user's paid plan merely to run tests.
4. Expose only the webhook route. Check the proxy flags with `python3 scripts/stripe-webhook-proxy.py --help`, start it on a free loopback port pointing at the local API, then run:

   ```sh
   ngrok http http://127.0.0.1:19090 --config=.local/stripe/ngrok.yml --url="$ngrok_domain" --log=.local/stripe/ngrok.log
   ```

   Set Stripe's endpoint to the public HTTPS URL plus `/webhooks/stripe`. An unsigned request must be rejected; unrelated paths must not expose the local Cloud API. The raw request body and Stripe-Signature header must survive unchanged. Run `python3 scripts/test-stripe-webhook-proxy.py`.

## Exercise payments in Chrome

Chrome or the app's browser pane both work. Sign in fresh users with `/auth/google/start?return_to=/app/billing&login_hint=<unique>` (the fixture derives `<hint>@fixture.invalid`), because the trial is once per person and per card.

- New subscription: complete Checkout, return to Cloud and verify plan, paid invoice, correct allowances and ledger. Redirect query strings alone must never grant funds.
- Trial: a user without a plan sees "Start free trial" (sidebar and Billing). Checkout runs in setup mode (save a card, nothing charged); after the webhook the plan reads "Free trial" with 2,000 credits and 5 compute hours, the sidebar meter updates without a reload, and the trial is no longer offered. Saving the same card on another account must not start a second trial: the wallet reports `trialRefused`, Billing explains it and an email says why.
- Automatic top-ups: with a saved card, turning them on opens a consent dialog naming the charge, threshold and daily limit. When included plus purchased AI credits are below the threshold, the sweep charges the card off-session; expect exactly one `auto_topup` payment, purchased credits up by the pack's face value and overage on. With `4000000000000341` saved, the charge is declined: top-ups turn off, Billing shows the reason, purchased credits are unchanged and exactly one "automatic top-up failed" email is written (the sweep and the `payment_intent.payment_failed` webhook must not both report it).
- Yearly plan: the Billing toggle shows the per-month price rounded like Checkout (Pro $16.67, "$200 billed yearly"); Checkout charges the yearly price; Billing shows "Paid through" a year out while allowances still reset monthly (month one granted by the invoice, later months by `annual_allowance_sweep`).
- Team: a personal account is sent to create a team; on the team account choose seats (minimum 2), Checkout shows the quantity, and the pooled allowance is per seat × seats. Add a seat in the portal: the prorated invoice is paid at once, seats and entitlement rise, and only the rest of the month's share is added. Invitations are capped by seats. Switching accounts in the account menu moves billing calls to the team; signing in as someone else forgets the previous user's team.
- Top-up: confirm face credits, service fee, and total charge before submission. A $22 charge buys 20,000 credits, not 22,000. Decline then retry; no grant on failure.
- 3DS: fail the challenge, verify no grant, retry and complete it. Check Cloud updates without reloading.
- Upgrade: preserve used and reserved credits; add only the prorated allowance difference. Verify the actual invoice line price, not stale metadata. A replay cannot reset usage or grant twice.
- Downgrade: verify the effective date in the portal; existing paid allowance remains available until period end. Cancel/reverse the scheduled change as cleanup if the account is kept for other tests.
- Cancel/resume: verify period-end cancellation, renewal status, current entitlement and portal return URL.
- Payment methods/invoices: open the portal, verify saved test payment method, invoice history and receipt detail.
- Partial/full refunds: compare face-value credit reversal to the refunded fraction of the charged amount, including the service fee. Repeated refund events must not debit twice.
- Asynchronous refunds: use Stripe’s `4000000000005126` test card. A refund initially succeeds and later emits `refund.failed`; restore only the credits actually debited, once. Replay success after failure and verify no second debit. Test an outage/redelivery through Stripe’s dashboard as well.
- Disputes: cover open, won, lost and out-of-order events with automated tests; report separately whether an actual Stripe sandbox dispute was run.

## Renewals, recovery and proof

Use a separate fixture user/customer for Stripe Test Clocks. A clock requires a newly created customer. If fixture-only DB setup is required to bind that customer, change only the new fixture account and record that setup explicitly. Anchor the clock in the past so the simulated renewal lands inside real wall-clock time. Set `pm_card_chargeCustomerFail` as the default before renewal: `past_due`, no allowance grant. Pay the failed invoice with `pm_card_visa`: active status and exactly one new period allowance. Cancel future fixture renewals.

A successful delivery is HTTP 2xx in Stripe and the ngrok inspector, a processed stored event, expected payment/period/ledger rows, and matching UI balances. Compare the two sides: `curl -s "http://127.0.0.1:4040/api/requests/http?limit=100"` lists the tunnel's requests (count `POST /webhooks/stripe` by status), and `SELECT kind, status, count(*), max(attempts) FROM stripe_events WHERE received_at > now() - interval '1 day' GROUP BY 1, 2` in the stack's database lists what was stored. `payment_intent.succeeded` for Checkout's own charges is `ignored` by design; only automatic top-up intents are handled. Check exact nanos and the double-entry invariant. During concurrent settlements `heldReserve` must equal the matching bucket reserves in the same wallet snapshot.

For a fixed handler, replay an already stored, signature-verified event with `cloud-worker --replay-stripe-event EVENT_ID` using the same isolated environment. Replay it twice and compare balances/ledger counts. Never fabricate a production webhook or edit a production balance to make a test pass.

Run Cloud store billing tests, API billing/event tests, integration signature/parser tests, frontend tests and the ten-step fixture journey (`scripts/e2e.sh`; step 10 covers the trial, a refused reused card, automatic top-up charge and decline, yearly and Team seats with fixture Stripe) against a separate fixture stack; add `HAARNESS_CLOUD_E2E_BROWSER=1` for the Playwright suite. Fixture card fingerprints and login hints must be unique per run, because the database outlives runs and a card claims one trial forever. Keep the real Stripe stack available for Chrome checks. Record source SHAs, sandbox ID, endpoint/domain, invoice/event IDs, before/after balances, test results and screenshots, without secrets. See `haarness-cloud/docs/BILLING-VERIFICATION-2026-09-27.md` and `BILLING-VERIFICATION-2026-10-05.md` for worked runs.

## Driving Stripe pages

- Checkout's card fields are iframes. Wait until the Apple Pay row has rendered (the form shifts down), then click each field by its on-screen position; reference-based clicks can land in the CVC field. Type the expiry as digits (`1234`) and check the field reads `12 / 34` before saying it is filled.
- A saved card is offered on the next Checkout for the same customer (`•••• 4242`), so subscriptions after a trial need no typing. A new team account is a new customer and needs the card again.
- Portal sessions expire after a few minutes of inactivity ("For security reasons, this page has expired"); open a new one from Billing → Manage plan.
- The portal prints dates in UTC; the app prints them in the browser's zone, so a period can end a day apart in each.

Official references: [Stripe testing](https://docs.stripe.com/testing), [Test clocks](https://docs.stripe.com/billing/testing/test-clocks), [webhooks](https://docs.stripe.com/webhooks), [ngrok agent](https://ngrok.com/docs/agent/).

## Explore beyond this guide

Treat the documented flows as a starting point. Do not hesitate to test relevant behavior that is not documented here: inspect the current implementation, follow unexpected results, and cover interactions and failure paths suggested by the task. Stay within the authorized environment and scope. Fix discovered bugs when authorized, verify the fixes, and record newly validated steps and remaining gaps so this skill improves with use.
