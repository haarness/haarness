import test from "node:test";
import assert from "node:assert/strict";
import {
  keyPlace,
  movesPrompts,
  promptTarget,
  stepIndex,
  stepsChats,
} from "../src/renderer/keyboard-nav";

/** An element inside the given ancestors (selectors it sits under). */
const at = (inside: string[], self = "") => ({
  closest: (selector: string) =>
    selector.split(",").some((part) => inside.includes(part.trim())) ? {} : null,
  matches: (selector: string) => selector === self,
});

test("the chat and prompt keys keep away from editors, terminals, overlays and drafts", () => {
  const composer = at(["textarea"], "[data-composer-input]");
  assert.equal(keyPlace(null, "", false), "page");
  assert.equal(keyPlace(at([]), "", false), "page");
  assert.equal(keyPlace(composer, "", false), "composer");
  assert.equal(keyPlace(composer, "  ", false), "composer");
  assert.equal(keyPlace(composer, "half a thought", false), "draft");
  assert.equal(keyPlace(at([".cm-editor", "[contenteditable]"]), "", false), "editor");
  assert.equal(keyPlace(at([".xterm", "textarea"]), "", false), "terminal");
  assert.equal(keyPlace(at(["input"]), "", false), "field");
  assert.equal(keyPlace(at([".action-menu"]), "", false), "overlay");
  assert.equal(keyPlace(composer, "", true), "overlay");
  // Alt and an arrow step chats from a draft; Mod and an arrow leave a draft's caret alone.
  assert.deepEqual(
    (["page", "composer", "draft", "field", "editor", "terminal", "overlay"] as const).map(
      (place) => [stepsChats(place), movesPrompts(place)],
    ),
    [
      [true, true],
      [true, true],
      [true, false],
      [false, false],
      [false, false],
      [false, false],
      [false, false],
    ],
  );
});

test("chat steps wrap past either end and start from an end with no open row", () => {
  assert.equal(stepIndex(3, 0, -1), 2);
  assert.equal(stepIndex(3, 2, 1), 0);
  assert.equal(stepIndex(3, 1, 1), 2);
  assert.equal(stepIndex(3, -1, 1), 0);
  assert.equal(stepIndex(3, -1, -1), 2);
  assert.equal(stepIndex(0, -1, 1), undefined);
});

test("prompt keys step from the prompt being read, reach either end and follow the newest output again", () => {
  // Four prompts, reading the second.
  assert.equal(promptTarget("previous", 4, 1, false), 0);
  assert.equal(promptTarget("next", 4, 1, false), 2);
  assert.equal(promptTarget("first", 4, 1, false), 0);
  assert.equal(promptTarget("last", 4, 1, false), "latest");
  // Past the last prompt is the newest output; before the first stays put.
  assert.equal(promptTarget("next", 4, 3, false), "latest");
  assert.equal(promptTarget("previous", 4, 0, false), 0);
  assert.equal(promptTarget("previous", 4, -1, false), undefined);
  assert.equal(promptTarget("next", 4, -1, false), 0);
  // At the newest output, Previous goes back to the last prompt.
  assert.equal(promptTarget("previous", 4, 3, true), 3);
  assert.equal(promptTarget("next", 4, 3, true), undefined);
  assert.equal(promptTarget("first", 0, -1, true), undefined);
});
