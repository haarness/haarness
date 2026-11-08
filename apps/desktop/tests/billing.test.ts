import { test } from "node:test";
import assert from "node:assert/strict";
import {
  cloudCanPay,
  credits,
  purchasedLabel,
  purchasedPays,
  spendableCredits,
  waitsForOverage,
  type BillingState,
} from "../src/billing";

type Summary = NonNullable<BillingState["summary"]>;
const summary = (fields: Partial<Summary>): Summary => ({
  accountId: "a", accountName: "Sam Rivera", plan: "No plan", accountStatus: "active",
  includedCredits: "0.000000", purchasedCredits: "0.000000", heldCredits: "0", cloudUnits: "0.00",
  cloudUnitsHeld: "0.00", cancelAtPeriodEnd: false, overageOptIn: false,
  billingUrl: "https://haarness.ai/app/billing", usageUrl: "https://haarness.ai/app/billing/usage",
  ...fields,
});

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
  const state = (fields: Partial<Summary>): BillingState => ({ loggedIn: true, summary: summary(fields) });
  assert.equal(cloudCanPay(state({})), false);
  assert.equal(cloudCanPay(state({ plan: "trial", trial: true, includedCredits: "2000.000000" })), true);
  assert.equal(cloudCanPay(state({ purchasedCredits: "20.000000" })), false, "an older Haarness: overage off");
  assert.equal(cloudCanPay(state({ purchasedCredits: "20.000000", purchasedPays: true })), true, "no plan");
  assert.equal(cloudCanPay(state({ plan: "pro", purchasedCredits: "20.000000", purchasedPays: false })), false);
  assert.equal(cloudCanPay(state({ purchasedCredits: "20.000000", purchasedPays: true, purchasedFrozen: true })), false);
  assert.equal(cloudCanPay(state({ purchasedCredits: "20.000000", overageOptIn: true })), true);
  assert.equal(cloudCanPay(state({ purchasedCredits: "20.000000", overageOptIn: true, purchasedFrozen: true })), false);
  assert.equal(cloudCanPay(state({ includedCredits: "5.000000", accountStatus: "suspended" })), false);
  assert.equal(cloudCanPay({ loggedIn: false }), true);
  assert.equal(cloudCanPay({ loggedIn: true }), true);
});

// Without a plan, purchased (or a new account's starter) credits pay as they are: they count toward the glance
// and never wait for overage. On a plan with overage off they still wait.
test("purchased credits count and wait as the account's billing says", () => {
  const starter = summary({ purchasedCredits: "1000.000000", purchasedPays: true, purchasedIsStarter: true });
  assert.equal(purchasedPays(starter), true);
  assert.equal(spendableCredits(starter), "1000");
  assert.equal(waitsForOverage(starter), false);
  assert.equal(purchasedLabel(starter), "Starter credits");
  assert.equal(purchasedLabel(summary({ purchasedCredits: "5.000000" })), "Purchased credits");

  const waiting = summary({ plan: "pro", includedCredits: "3.000000", purchasedCredits: "20.000000", purchasedPays: false });
  assert.equal(spendableCredits(waiting), "3.000000");
  assert.equal(waitsForOverage(waiting), true);
  assert.equal(waitsForOverage({ ...waiting, overageOptIn: true, purchasedPays: true }), false);
  assert.equal(waitsForOverage({ ...waiting, purchasedFrozen: true }), false, "on hold, overage would not help");

  // An older Haarness says nothing: purchased credits pay only with overage on and not on hold.
  const older = summary({ includedCredits: "3.000000", purchasedCredits: "20.000000", overageOptIn: true });
  assert.equal(purchasedPays(older), true);
  assert.equal(spendableCredits(older), "23");
  assert.equal(purchasedPays({ ...older, purchasedFrozen: true }), false);
  assert.equal(waitsForOverage({ ...older, overageOptIn: false }), true);
});
