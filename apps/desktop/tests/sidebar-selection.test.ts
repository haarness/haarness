import test from "node:test";
import assert from "node:assert/strict";
import {
  emptySelection,
  pickAll,
  pickChat,
  pickTree,
  settle,
  startsPick,
} from "../src/renderer/sidebar-selection";

const click = (fields: Partial<MouseEvent> = {}) => ({
  button: 0,
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  ...fields,
});

test("Command-click on macOS and Control-click elsewhere start picking", () => {
  assert.equal(startsPick(click({ metaKey: true }), true), true);
  assert.equal(startsPick(click({ ctrlKey: true }), true), false);
  assert.equal(startsPick(click({ ctrlKey: true }), false), true);
  assert.equal(startsPick(click({ metaKey: true }), false), false);
  assert.equal(startsPick(click(), true), false);
  assert.equal(startsPick(click({ metaKey: true, shiftKey: true }), true), false);
  assert.equal(startsPick(click({ metaKey: true, button: 1 }), true), false);
});

test("a click picks or drops a row, Shift picks the rows since the last click, and Select all shown adds the rest", () => {
  const drawn = ["a", "b", "c", "d", "e"];
  let selection = emptySelection("project");
  selection = pickChat(selection, "b", false, drawn);
  assert.deepEqual(selection.chats, ["b"]);
  selection = pickChat(selection, "d", true, drawn);
  assert.deepEqual(selection.chats, ["b", "c", "d"]);
  // Upwards from the same anchor, keeping what is picked.
  selection = pickChat(selection, "a", true, drawn);
  assert.deepEqual(selection.chats, ["b", "c", "d", "a"]);
  selection = pickChat(selection, "c", false, drawn);
  assert.deepEqual(selection.chats, ["b", "d", "a"]);
  assert.equal(selection.anchor, "c");
  // An anchor no longer drawn (its project collapsed) makes Shift a plain click.
  selection = pickChat(selection, "e", true, ["e"]);
  assert.deepEqual(selection.chats, ["b", "d", "a", "e"]);
  selection = pickAll(selection, drawn);
  assert.deepEqual([...selection.chats].sort(), drawn);
});

test("worktrees are picked apart from chats, and what an action handled leaves the selection", () => {
  let selection = pickTree(emptySelection("project"), "tree-1");
  selection = pickChat(selection, "a", false, ["a", "b"]);
  assert.deepEqual(selection.trees, ["tree-1"]);
  assert.deepEqual(pickTree(selection, "tree-1").trees, []);
  const left = settle(selection, { chats: ["a"] });
  assert.deepEqual(left, { ...selection, chats: [], trees: ["tree-1"] });
  assert.equal(settle(left!, { trees: ["tree-1"] }), null);
});
