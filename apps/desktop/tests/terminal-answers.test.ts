import test from "node:test";
import assert from "node:assert/strict";
import { terminalAnswers } from "../src/main/terminal-answers";

const terminal = "local:/p:pty";

test("of two windows on one terminal, only the one used last answers its programs", () => {
  const answers = terminalAnswers();
  const connected = new Set([1, 2]);
  const live = (win: number) => connected.has(win);
  // Both windows connect (after a server restart, say); the later one answers.
  answers.use(terminal, 1);
  answers.use(terminal, 2);
  assert.equal(answers.answers(terminal, 1, live), false);
  assert.equal(answers.answers(terminal, 2, live), true);
  // The person types in the first window: its answers go on from now on.
  answers.use(terminal, 1);
  assert.equal(answers.answers(terminal, 1, live), true);
  assert.equal(answers.answers(terminal, 2, live), false);
  // Its panel goes away: the window still connected answers.
  connected.delete(1);
  assert.equal(answers.answers(terminal, 2, live), true);
  // A window closed is forgotten.
  answers.forget(2);
  connected.add(1);
  assert.equal(answers.answers(terminal, 1, live), true);
});

test("each terminal has its own answering window, and one window alone always answers", () => {
  const answers = terminalAnswers();
  const live = () => true;
  answers.use(terminal, 1);
  answers.use("local:/p:other", 2);
  answers.use("local:/p:other", 1);
  assert.equal(answers.answers(terminal, 1, live), true);
  assert.equal(answers.answers("local:/p:other", 2, live), false);
  // A terminal no window used yet stops no answer.
  assert.equal(answers.answers("local:/p:unknown", 3, live), true);
});
