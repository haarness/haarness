import test from "node:test";
import assert from "node:assert/strict";
import { parseAccess, pressAction, refreshAfter } from "../src/renderer/remote-access";

test("the computer's remote access answer is checked before the title bar trusts it", () => {
  assert.deepEqual(
    parseAccess({ state: "on", computer: "Desk", organization: "Studio", error: null }),
    { state: "on", computer: "Desk", organization: "Studio", error: undefined },
  );
  // An unknown state, blank text or no answer at all reads as off.
  assert.deepEqual(parseAccess({ state: "sharing", computer: " ", error: 7 }), {
    state: "off",
    computer: undefined,
    organization: undefined,
    error: undefined,
  });
  assert.equal(parseAccess(null).state, "off");
  assert.equal(parseAccess({ state: "signed_out" }).state, "signed_out");
});

test("a press signs in first, turns access off while it is on or on its way, and on otherwise", () => {
  assert.equal(pressAction({ state: "signed_out" }), "sign-in");
  assert.equal(pressAction({ state: "on" }), "off");
  assert.equal(pressAction({ state: "connecting" }), "off");
  assert.equal(pressAction({ state: "off" }), "on");
  // A connector that gave up is tried again.
  assert.equal(pressAction({ state: "off", error: "Remote host access was revoked" }), "on");
  // Before the first answer the server decides, signing in if it must.
  assert.equal(pressAction(undefined), "on");
});

test("the state is read again soon while connecting, now and then while on, never while off", () => {
  assert.equal(refreshAfter({ state: "connecting" }), 2_000);
  assert.equal(refreshAfter({ state: "on" }), 30_000);
  assert.equal(refreshAfter({ state: "off" }), 0);
  assert.equal(refreshAfter({ state: "signed_out" }), 0);
  assert.equal(refreshAfter(undefined), 0);
});
