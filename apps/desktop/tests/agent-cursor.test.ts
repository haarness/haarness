import test from "node:test";
import assert from "node:assert/strict";
import { cursorLabel, cursorLabelKeys } from "../src/main/browser/agent-cursor";

test("the agent cursor names its target, briefly, in the interface language", () => {
  assert.equal(cursorLabel("click", "Place order"), "Clicking “Place order”");
  assert.equal(cursorLabel("click"), "Clicking");
  assert.equal(cursorLabel("type", "  Full\n  name "), "Typing in “Full name”");
  assert.equal(
    cursorLabel("check", "I accept the terms and conditions of this demo store"),
    "Checking “I accept the terms and…”",
  );
  assert.equal(cursorLabel("click", "Supercalifragilisticexpialidocious-button"), "Clicking “Supercalifragilisticexpialidoci…”");
  const french: Record<string, string> = { "Clicking “{name}”": "Clic sur « {name} »", Clicking: "Clic" };
  const translate = (key: string) => french[key] ?? key;
  assert.equal(cursorLabel("click", "Payer", translate), "Clic sur « Payer »");
  assert.equal(cursorLabel("click", "", translate), "Clic");
  // Every key a label can use is listed for the catalog guard.
  for (const key of ["Clicking “{name}”", "Pressing {key}", "Scrolling down"])
    assert.ok(cursorLabelKeys.includes(key), key);
});
