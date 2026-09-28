---
name: haarness-email-sendgrid
description: Validate Haarness Cloud email authentication and delivery through SendGrid, Cloudflare DNS, account signup and password reset, and queued operational notifications.
---

# Haarness email and SendGrid

Use the active Cloud checkout beside the native repository. Preserve dirty checkouts and reuse their build cache when working in an isolated checkout. `haarness.ai` is the public domain.

## Discover the actual email surface

Search current code for `Notification {`, `notifier.send`, and `enqueue_notification`; do not infer implemented flows from enum comments or old reports. At the time this skill was validated:

- `cloud-api/src/routes/email_auth.rs`: signup verification and password reset, sent synchronously. A link has a 30-minute expiry, is single-use, and carries its token in a URL fragment. Only a verified link creates the account. Delivery errors must not report success or leak the token/provider response.
- `cloud-worker/src/jobs/workspace_common.rs`: verified owner/billing recipients enter the durable notification queue. Producers are compute allowance exhaustion, unfunded storage, and deletion after the storage grace period. `notifications.rs` renders and sends them; a completed delivery is not resent on job replay.
- `payment_failed` and `allowance_low` appear in examples/tests but are not currently production email producers. Stripe's own receipts and billing emails are a separate configuration surface.

Native clients use the same Cloud account flow. Router has no independent SendGrid transport. Recheck these boundaries when code changes. Explore relevant undocumented scenarios too; this skill is a starting point, not the limit of coverage.

## Configure and verify the sender

Read only the required values from the private local environment; never print credentials. Both API and worker need `HAARNESS_CLOUD_EMAIL_PROVIDER=sendgrid`, `HAARNESS_CLOUD_SENDGRID_API_KEY`, and `HAARNESS_CLOUD_EMAIL_FROM`. Current intended sender is `noreply@haarness.ai`, reply-to `admin@haarness.ai`. Confirm that reply-to is a working mailbox before claiming replies work.

When browser setup is requested, use Chrome control for Cloudflare, SendGrid, and Gmail. If control reports `User unavailable`, report that concrete blocker; continue independent code/API diagnostics. Do not claim to have visited or configured those pages.

Use SendGrid's authenticated-domain list to find the existing `haarness.ai` entry before creating one. Read and use exactly the provider-generated DNS records. Cloudflare email CNAMEs must be DNS-only, not proxied. Preserve Google/other mail MX and SPF records; automated SendGrid security uses a return-path subdomain and DKIM CNAMEs, so it does not require replacing the root SPF record. Inspect existing DKIM selectors and DMARC before changing them. Avoid changing defaults for other products in a shared SendGrid account.

After DNS changes, resolve each record publicly and run SendGrid's domain validation. Record the domain ID and per-record result without credentials. A created domain whose `valid` is false is unfinished. Do not create duplicate domain entries on retry.

Official references: [domain authentication API](https://www.twilio.com/docs/sendgrid/api-reference/domain-authentication/authenticate-a-domain), [validation API](https://www.twilio.com/docs/sendgrid/api-reference/domain-authentication/validate-a-domain-authentication), [mail send](https://www.twilio.com/docs/sendgrid/api-reference/mail-send/mail-send).

## Automated proof

From Cloud, use a dedicated test PostgreSQL database via `HAARNESS_CLOUD_TEST_DATABASE_URL`; tests silently skip when it is absent. These tests do not send real email:

```sh
cargo test -p cloud-integrations --test sendgrid
cargo test -p cloud-api --test email_accounts -- --test-threads=4
cargo test -p cloud-worker --test job_loop -- --test-threads=4
```

The shared loopback fixture is `tests/support/sendgrid.rs`. Verify real adapter requests, sender/recipient, unchanged fragment links, one-time use, password reset invalidating the old password, safe rejection errors, queue retries, verified-recipient filtering, and no resend after completion. Keep click tracking disabled in each transactional request, including `enable_text`; inherited account-wide tracking was enabled in the live account when this was discovered. Also disable open/Google Analytics tracking for these messages.

Use the existing lifecycle suites when changing notification producers, and billing suites when changing payment events. Do not label manually enqueued fixtures as proof of a production trigger.

## Live browser and inbox proof

Use only the mailbox explicitly authorized by the user. Identify it in their Gmail session instead of guessing from Git metadata. Use a disposable Haarness test account or authorized plus alias so a password-reset test does not change their normal account. Keep synthetic test credentials separate from real credentials; follow the browser tool's handoff rules for password entry.

Run an isolated API/worker fixture or an explicitly selected test environment with the real notifier. Do not switch a shared fixture stack to SendGrid while it has queued `example.invalid` or fixture-user recipients. For queued tests, scope every send to the authorized recipient and stop the owned worker afterward. A notification's `sent` state and SendGrid HTTP 202 mean provider acceptance, not confirmed delivery.

Two opt-in fixtures are available in Cloud. Neither runs during the normal suite:

```sh
# Provide real SendGrid settings and the disposable test database in the process environment.
# Choose an absent stop-file path first. The API listens on 19188; Vite uses 19189.
HAARNESS_EMAIL_TEST_STOP_FILE=/absolute/private/path/email-test.stop \
  cargo test -p cloud-api --test email_accounts live_sendgrid_browser_fixture -- --ignored --exact --nocapture
# Set HAARNESS_EMAIL_TEST_RECIPIENT to the authorized mailbox and HAARNESS_EMAIL_TEST_RUN to a unique label.
cargo test -p cloud-worker --test job_loop live_sendgrid_queued_notifications -- --ignored --exact --nocapture
```

Start Vite with `HAARNESS_CLOUD_API_ORIGIN=http://127.0.0.1:19188 npm --prefix web run dev -- --port 19189`.
Create the configured stop file when finished. Require a zero test exit and removal of the printed owned
schema. Interrupting Cargo can skip Rust destructors, so Ctrl-C alone is not cleanup proof. The browser
fixture has a 45-minute deadline as a fallback. If a run crashes, verify the exact printed schema and test
account before deleting that schema; never sweep unrelated `test_*` schemas. Stop the owned frontend too.

Exercise signup from the web, inspect the arriving message and its original headers in Gmail, follow the unchanged verification link, finish signup, sign in, request reset, follow that message, and prove the old password no longer works. Check expired/reused links and provider rejection using the hermetic suite rather than repeatedly sending real emails. Per-address mail requests are limited to five per 15 minutes.

For each operational kind, exercise the producer in a disposable funded/unfunded session fixture where feasible; otherwise identify the narrower queue-only proof explicitly. Inspect subject/body and confirm arrival. In Gmail's original headers require aligned DKIM and DMARC/SPF results for `haarness.ai`; a message in spam or merely accepted by SendGrid is not an inbox-delivery success. Inspect SendGrid activity for rejected, bounced, or deferred messages before retrying.

Report separately: code tests, DNS records published, provider validation, actual sends accepted, inbox/header proof, and visible browser journeys. Never erase an unverified step with a blanket end-to-end success claim.
