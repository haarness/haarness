import { test } from "node:test";
import assert from "node:assert/strict";
import { cloudCanPay, credits, type BillingState } from "../src/billing";

test("credit formatting retains large integers and never rounds up spendable balances", () => {
  assert.equal(credits("9007199254740993.123456"), `${BigInt("9007199254740993").toLocaleString()}.12`);
  assert.equal(credits("0.000001"), "<0.01");
  assert.equal(credits("0.009999"), "<0.01");
  assert.equal(credits("19.999999"), "19.99");
  assert.equal(credits("0"), "0");
  assert.equal(credits("1.200000"), "1.2");
  assert.equal(credits("NaN"), "—");
});

test("Cloud can pay with a credit that pays now; signed out or loading, nothing is known", () => {
  const state = (summary: Partial<NonNullable<BillingState["summary"]>>): BillingState => ({
    loggedIn: true,
    summary: {
      accountId: "a", accountName: "Sam Rivera", plan: "No plan", accountStatus: "active",
      includedCredits: "0.000000", purchasedCredits: "0.000000", heldCredits: "0", cloudUnits: "0.00",
      cloudUnitsHeld: "0.00", cancelAtPeriodEnd: false, overageOptIn: false,
      billingUrl: "https://haarness.ai/app/billing", usageUrl: "https://haarness.ai/app/billing/usage",
      ...summary,
    },
  });
  assert.equal(cloudCanPay(state({})), false);
  assert.equal(cloudCanPay(state({ plan: "trial", trial: true, includedCredits: "2000.000000" })), true);
  assert.equal(cloudCanPay(state({ purchasedCredits: "20.000000" })), false, "overage off");
  assert.equal(cloudCanPay(state({ purchasedCredits: "20.000000", overageOptIn: true })), true);
  assert.equal(cloudCanPay(state({ purchasedCredits: "20.000000", overageOptIn: true, purchasedFrozen: true })), false);
  assert.equal(cloudCanPay(state({ includedCredits: "5.000000", accountStatus: "suspended" })), false);
  assert.equal(cloudCanPay({ loggedIn: false }), true);
  assert.equal(cloudCanPay({ loggedIn: true }), true);
});
