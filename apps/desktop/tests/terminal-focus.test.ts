import test from "node:test";
import assert from "node:assert/strict";
import { keyboardClaim, typedElsewhere } from "../src/renderer/terminal-focus";

// A terminal view takes the keyboard on its first connect only when the
// person brought it up. Each step below is what the panel tells the claim.

test("a terminal the person opens, picks or starts takes the keyboard once it connects, and only once", () => {
  const claim = keyboardClaim();
  // The panel opened: its first terminal connects.
  claim.claim();
  assert.equal(claim.take(), true);
  // A view that connects later by itself (another window's, a remount) does not.
  assert.equal(claim.take(), false);
  // A tab clicked: its press ends any earlier claim, its click makes one.
  claim.release();
  claim.claim();
  assert.equal(claim.take(), true);
});

test("a pointer press before the terminal connects leaves the keyboard where it was pressed", () => {
  const claim = keyboardClaim();
  claim.claim();
  claim.release();
  assert.equal(claim.take(), false);
});

test("another chat's terminal shown with the panel open leaves the keyboard in that chat's message box", () => {
  const claim = keyboardClaim();
  // Nothing brought it up: switching chats lists that chat's terminals.
  claim.shown(false, true);
  assert.equal(claim.take(), false);
  // Opened, then another chat chosen from the keyboard before it connected.
  claim.claim();
  claim.shown(false, true);
  assert.equal(claim.take(), false);
  // Chosen from Search while the keyboard was in the terminal: the chat's
  // message box takes it, not the new chat's shell.
  claim.shown(true, true);
  assert.equal(claim.take(), false);
});

test("a terminal that takes the place of the one shown has the keyboard if the panel had it", () => {
  const claim = keyboardClaim();
  // The one shown was closed from its tab, or its server was replaced, while
  // the keyboard was in the panel.
  claim.shown(true);
  assert.equal(claim.take(), true);
  // While the keyboard was in the message box.
  claim.shown(false);
  assert.equal(claim.take(), false);
  // The same place listed again before the first terminal connected keeps
  // the claim of the panel just opened.
  claim.claim();
  claim.shown(false);
  assert.equal(claim.take(), true);
});

test("typing or Tab outside the terminal keeps the keyboard there; a shortcut may still bring the terminal up", () => {
  const key = (key: string, mods: Partial<{ ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean }> = {}) => ({
    key, ctrlKey: false, metaKey: false, altKey: false, ...mods,
  });
  for (const event of [key("a"), key("A", { shiftKey: true }), key(" "), key("Enter"), key("Backspace"), key("Delete"), key("Tab"), key("Tab", { shiftKey: true }), key("é")])
    assert.equal(typedElsewhere(event), true, event.key);
  for (const event of [key("j", { metaKey: true }), key("`", { ctrlKey: true }), key("k", { metaKey: true }), key("ArrowDown", { altKey: true }), key("Shift"), key("Meta"), key("ArrowUp"), key("Escape"), key("Process")])
    assert.equal(typedElsewhere(event), false, event.key);
  // Typing in the message box while the terminal connects ends the claim.
  const claim = keyboardClaim();
  claim.claim();
  if (typedElsewhere(key("x"))) claim.release();
  assert.equal(claim.take(), false);
});
