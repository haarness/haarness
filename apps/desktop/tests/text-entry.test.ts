import test from "node:test";
import assert from "node:assert/strict";
import { composing, isTextEntry } from "../src/renderer/text-entry";

const field = (tagName: string, extra: Record<string, unknown> = {}) => ({
  tagName,
  isContentEditable: false,
  ...extra,
});

test("text entry covers inputs, textareas and editable content, not buttons or toggles", () => {
  assert.equal(isTextEntry(field("INPUT")), true);
  assert.equal(isTextEntry(field("INPUT", { type: "search" })), true);
  assert.equal(isTextEntry(field("TEXTAREA")), true);
  // A code editor or a rich field.
  assert.equal(isTextEntry(field("DIV", { isContentEditable: true })), true);
  for (const type of ["checkbox", "radio", "range", "button", "file"])
    assert.equal(isTextEntry(field("INPUT", { type })), false, type);
  assert.equal(isTextEntry(field("TEXTAREA", { readOnly: true })), false);
  assert.equal(isTextEntry(field("BUTTON")), false);
  assert.equal(isTextEntry(field("BODY")), false);
  assert.equal(isTextEntry(null), false);
});

test("a key that ends a composition belongs to the input method, by its flag or its keyCode", () => {
  assert.equal(composing({ isComposing: true, keyCode: 27 }), true);
  // The key confirming a candidate can come after the flag has gone false.
  assert.equal(composing({ isComposing: false, keyCode: 229 }), true);
  assert.equal(composing({ isComposing: false, keyCode: 27 }), false);
  assert.equal(composing({ isComposing: false, keyCode: 13 }), false);
});
