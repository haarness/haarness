import test from "node:test";
import assert from "node:assert/strict";
import { chatTitle, editableTitle } from "../src/renderer/chat-title";
import { untitledKind } from "../src/chat-titles";

test("a chat without a generated title reads as untitled, never as the server's thread", () => {
  assert.equal(chatTitle("New thread"), "Untitled chat");
  assert.equal(chatTitle("New chat"), "Untitled chat");
  assert.equal(chatTitle(""), "Untitled chat");
  assert.equal(chatTitle("   "), "Untitled chat");
  assert.equal(chatTitle(undefined), "Untitled chat");
  assert.equal(chatTitle("Fix the login flow"), "Fix the login flow");
});

test("a fork of an untitled chat says so in the reader's words, from either server wording", () => {
  for (const title of ["Fork of New thread", "Fork of New chat", "Fork of Fork of New thread", "Fork of    "])
    assert.equal(chatTitle(title), "Fork of untitled chat", title);
  assert.equal(chatTitle("Fork of Fix the login flow"), "Fork of Fix the login flow");
  assert.equal(untitledKind("Fork of New threads"), undefined);
  assert.equal(untitledKind("Forked a New thread"), undefined);
});

test("a rename starts from the shown name, or empty for an untitled chat", () => {
  assert.equal(editableTitle("New thread"), "");
  assert.equal(editableTitle("Fork of New thread"), "Fork of untitled chat");
  assert.equal(editableTitle("Fix the login flow"), "Fix the login flow");
});
