import test from "node:test";
import assert from "node:assert/strict";
import { chatTitle } from "../src/renderer/chat-title";

test("a chat without a generated title reads as untitled, never as the server's thread", () => {
  assert.equal(chatTitle("New thread"), "Untitled chat");
  assert.equal(chatTitle(""), "Untitled chat");
  assert.equal(chatTitle(undefined), "Untitled chat");
  assert.equal(chatTitle("Fix the login flow"), "Fix the login flow");
});
