---
name: haarness-payments-ngrok
description: Configure and test Haarness Cloud Stripe sandbox payments through ngrok, including checkout, portal, refunds, subscription changes, webhook recovery and ledger proof.
---

# Stripe and ngrok payment verification

Run from the Haarness workspace root, which contains `haarness/`, `haarness-cloud/`, and `haarness-router/`. Use a clean checkout or worktree for source changes; preserve any existing Cloud UI work. Read the native `.skills/haarness-build-run/SKILL.md` before rebuilding native binaries.

Use an isolated Stripe sandbox, local database, ports and fixture user. Never run payment fixtures against live Stripe keys or a production database. Test cards are `4242424242424242` (success), `4000000000009995` (insufficient funds), and `4000002500003155` (3DS). Use a future expiry and test CVC. In API tests prefer Stripe's documented test PaymentMethod IDs. Confirm the current Stripe account ID with the API before setup. Keep keys, signing secrets, OAuth tokens and ngrok tokens in ignored files with mode 0600; do not capture them in screenshots or logs.

## Configure the complete path

1. In Chrome, select/create the intended Stripe sandbox. Products and prices must match `haarness-cloud/config/plans.v1.json`. Run the following commands from the selected Cloud checkout (or worktree). Use the repeatable setup helper:

   ```sh
   python3 scripts/stripe-sandbox.py --help
   python3 scripts/stripe-sandbox.py --key-file .local/stripe/secret-key --account "$stripe_sandbox_account" --webhook-url "$stripe_webhook_url" --output .local/stripe
   ```

2. Pin the Stripe webhook wire version to `2025-08-27.basil`. The setup script configures the required events, monthly products, top-ups and customer portal. Upgrades invoice immediately; downgrades take effect at period end. Load the generated price bindings, secret key and signing secret into the isolated API/worker process environment, never into the frontend bundle.
3. Run `ngrok version`, then `ngrok update` if needed. Preserve the user's global ngrok configuration and existing tunnels. Use a private dedicated config for the selected dashboard account/domain. Do not change the user's paid plan merely to run tests.
4. Expose only the webhook route. Check the proxy flags with `python3 scripts/stripe-webhook-proxy.py --help`, start it on a free loopback port pointing at the local API, then run:

   ```sh
   ngrok http http://127.0.0.1:19090 --config=.local/stripe/ngrok.yml --url="$ngrok_domain" --log=.local/stripe/ngrok.log
   ```

   Set Stripe's endpoint to the public HTTPS URL plus `/webhooks/stripe`. An unsigned request must be rejected; unrelated paths must not expose the local Cloud API. The raw request body and Stripe-Signature header must survive unchanged. Run `python3 scripts/test-stripe-webhook-proxy.py`.

## Exercise payments in Chrome

- New subscription: complete Checkout, return to Cloud and verify plan, paid invoice, correct allowances and ledger. Redirect query strings alone must never grant funds.
- Top-up: confirm face credits, funding fee, and total charge before submission. A $22 charge buys 20,000 credits, not 22,000. Decline then retry; no grant on failure.
- 3DS: fail the challenge, verify no grant, retry and complete it. Check Cloud updates without reloading.
- Upgrade: preserve used and reserved credits; add only the prorated allowance difference. Verify the actual invoice line price, not stale metadata. A replay cannot reset usage or grant twice.
- Downgrade: verify the effective date in the portal; existing paid allowance remains available until period end. Cancel/reverse the scheduled change as cleanup if the account is kept for other tests.
- Cancel/resume: verify period-end cancellation, renewal status, current entitlement and portal return URL.
- Payment methods/invoices: open the portal, verify saved test payment method, invoice history and receipt detail.
- Partial/full refunds: compare face-value credit reversal to the refunded fraction of the charged amount, including funding fee. Repeated refund events must not debit twice.
- Asynchronous refunds: use Stripe’s `4000000000005126` test card. A refund initially succeeds and later emits `refund.failed`; restore only the credits actually debited, once. Replay success after failure and verify no second debit. Test an outage/redelivery through Stripe’s dashboard as well.
- Disputes: cover open, won, lost and out-of-order events with automated tests; report separately whether an actual Stripe sandbox dispute was run.

## Renewals, recovery and proof

Use a separate fixture user/customer for Stripe Test Clocks. A clock requires a newly created customer. If fixture-only DB setup is required to bind that customer, change only the new fixture account and record that setup explicitly. Anchor the clock in the past so the simulated renewal lands inside real wall-clock time. Set `pm_card_chargeCustomerFail` as the default before renewal: `past_due`, no allowance grant. Pay the failed invoice with `pm_card_visa`: active status and exactly one new period allowance. Cancel future fixture renewals.

A successful delivery is HTTP 2xx in Stripe and the ngrok inspector, a processed stored event, expected payment/period/ledger rows, and matching UI balances. Check exact nanos and the double-entry invariant. During concurrent settlements `heldReserve` must equal the matching bucket reserves in the same wallet snapshot.

For a fixed handler, replay an already stored, signature-verified event with `cloud-worker --replay-stripe-event EVENT_ID` using the same isolated environment. Replay it twice and compare balances/ledger counts. Never fabricate a production webhook or edit a production balance to make a test pass.

Run Cloud store billing tests, API billing/event tests, integration signature/parser tests, frontend tests and the nine-step fixture journey (`scripts/e2e.sh`) against a separate fixture stack. Keep the real Stripe stack available for Chrome checks. Record source SHAs, sandbox ID, endpoint/domain, invoice/event IDs, before/after balances, test results and screenshots, without secrets. See `haarness-cloud/docs/BILLING-VERIFICATION-2026-09-27.md` for the worked run.

Official references: [Stripe testing](https://docs.stripe.com/testing), [Test clocks](https://docs.stripe.com/billing/testing/test-clocks), [webhooks](https://docs.stripe.com/webhooks), [ngrok agent](https://ngrok.com/docs/agent/).
