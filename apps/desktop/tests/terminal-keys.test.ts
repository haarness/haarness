import test from "node:test";
import assert from "node:assert/strict";
import { optionKeys } from "../src/renderer/terminal-keys";

const press = (code: string, altKey = true) => ({ type: "keydown", code, altKey });
const release = (code: string) => ({ type: "keyup", code, altKey: false });

test("Option sends Meta only as chosen, and Left Option keeps the right one for characters", () => {
  const keys = optionKeys();
  // Off (the default): Option types what the layout composes.
  keys.meta(press("AltLeft"), "off");
  assert.equal(keys.meta(press("KeyB"), "off"), false);
  keys.meta(release("AltLeft"), "off");

  // Left Option: the left key is Meta, the right one still types ∫ or @.
  keys.meta(press("AltLeft"), "left");
  assert.equal(keys.meta(press("KeyB"), "left"), true);
  keys.meta(release("AltLeft"), "left");
  keys.meta(press("AltRight"), "left");
  assert.equal(keys.meta(press("KeyL"), "left"), false);
  // Both held: the left one wins.
  keys.meta(press("AltLeft"), "left");
  assert.equal(keys.meta(press("KeyL"), "left"), true);
  keys.meta(release("AltLeft"), "left");
  keys.meta(release("AltRight"), "left");

  // Both Option keys.
  keys.meta(press("AltRight"), "both");
  assert.equal(keys.meta(press("KeyB"), "both"), true);
});

test("an Option release missed in the background does not stick", () => {
  const keys = optionKeys();
  keys.meta(press("AltRight"), "left");
  // Back in the window with the left key now: the stale right key is gone.
  assert.equal(keys.meta(press("KeyX", false), "left"), true);
  keys.meta(press("AltRight"), "left");
  keys.reset();
  assert.equal(keys.meta(press("KeyB"), "left"), true);
});
