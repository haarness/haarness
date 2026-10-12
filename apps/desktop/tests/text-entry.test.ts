import test from "node:test";
import assert from "node:assert/strict";
import { isTextEntry, typingElsewhere } from "../src/renderer/text-entry";

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

test("an open-ended question takes the focus unless someone is typing outside it", () => {
  const answer = field("INPUT", { value: "" });
  const dock = { contains: (node: unknown) => node === answer };
  const composer = field("TEXTAREA", { value: "" });
  // Nothing typed anywhere: the transcript, a button or nothing has the focus.
  assert.equal(typingElsewhere(field("BODY"), dock, composer), false);
  assert.equal(typingElsewhere(field("BUTTON"), dock, composer), false);
  // An empty message box stays in view during a question but is idle.
  assert.equal(typingElsewhere(composer, dock, composer), false);
  // A draft in progress keeps the focus, and so does any other field, even
  // empty: a terminal's input, a comment box or a code editor.
  const drafting = field("TEXTAREA", { value: "half a thought" });
  assert.equal(typingElsewhere(drafting, dock, drafting), true);
  assert.equal(typingElsewhere(field("TEXTAREA", { value: "" }), dock, composer), true);
  assert.equal(typingElsewhere(field("DIV", { isContentEditable: true }), dock, composer), true);
  // Already inside the question.
  assert.equal(typingElsewhere(answer, dock, composer), false);
});
