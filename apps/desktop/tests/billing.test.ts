import { test } from "node:test";
import assert from "node:assert/strict";
import { credits } from "../src/billing";

test("credit formatting retains large integers and never rounds up spendable balances", () => {
  assert.equal(credits("9007199254740993.123456"), `${BigInt("9007199254740993").toLocaleString()}.12`);
  assert.equal(credits("0.000001"), "<0.01");
  assert.equal(credits("0.009999"), "<0.01");
  assert.equal(credits("19.999999"), "19.99");
  assert.equal(credits("0"), "0");
  assert.equal(credits("1.200000"), "1.2");
  assert.equal(credits("NaN"), "—");
});
