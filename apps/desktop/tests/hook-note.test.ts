import test from "node:test";
import assert from "node:assert/strict";
import { hookFeedback, hookNote } from "../src/renderer/hook-note";

test("a hook's message reads as its event and the feedback in its block", () => {
  const text = "Read this first.\n\n<hook-feedback>\nrun the tests\nthen lint\n</hook-feedback>";
  assert.deepEqual(hookNote({ type: "user_message", text, synthetic: true, hook: "Stop" }), {
    event: "Stop",
    text: "run the tests\nthen lint",
  });
  // Every other message shows as it always did.
  assert.equal(hookNote({ type: "user_message", text, synthetic: true }), undefined);
  assert.equal(hookNote({ type: "user_message", text: "hi" }), undefined);
  assert.equal(hookNote({ type: "assistant_message", text }), undefined);
});

test("a coloured hook output shows without its escape sequences, even one cut short", () => {
  const text = "<hook-feedback>\n\u001b[31mFAIL\u001b[0m src/parse.test.ts\n\u001b]0;title\u0007two\u001b[3\n</hook-feedback>";
  assert.deepEqual(hookNote({ type: "user_message", text, synthetic: true, hook: "Stop" }), {
    event: "Stop",
    // A sequence cut short loses its escape byte, so what is left is text.
    text: "FAIL src/parse.test.ts\ntwo[3",
  });
});

test("feedback that writes the closing tag itself keeps it, and text without a block is all feedback", () => {
  assert.equal(hookFeedback('<hook-feedback event="Stop">\na </hook-feedback> b\n</hook-feedback>'), "a </hook-feedback> b");
  assert.equal(hookFeedback(" plain words "), "plain words");
  assert.equal(hookFeedback("<hook-feedback unclosed"), "<hook-feedback unclosed");
});
