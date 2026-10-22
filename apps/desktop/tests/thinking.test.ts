import test from "node:test";
import assert from "node:assert/strict";
import { savedThinking } from "../src/shared";
import { latestThought, thoughtDuration } from "../src/renderer/thinking";

test("the thinking display starts Compact, and the old switch turned on becomes Show", () => {
  assert.equal(savedThinking(undefined), "compact");
  assert.equal(savedThinking({}), "compact");
  // The switch was off by default and saved with every preference, so off
  // cannot be told from untouched.
  assert.equal(savedThinking({ showReasoning: false }), "compact");
  assert.equal(savedThinking({ showReasoning: true }), "show");
  for (const value of ["compact", "show", "hide"] as const)
    assert.equal(savedThinking({ thinking: value, showReasoning: true }), value);
  assert.equal(savedThinking({ thinking: "loud" }), "compact");
});

test("Compact ticks through the newest line of thought without its Markdown marks", () => {
  assert.equal(latestThought(""), "");
  assert.equal(latestThought("Reading the request.\n\n## **Weighing** two `fixes`\n"), "Weighing two fixes");
  assert.equal(latestThought("- first\n> quoted line"), "quoted line");
  assert.equal(latestThought("partial line still stre"), "partial line still stre");
});

test("a thought says how long it took in whole seconds, in the interface language", () => {
  assert.equal(thoughtDuration(120, "en"), "1s");
  assert.equal(thoughtDuration(12_400, "en"), "12s");
  assert.equal(thoughtDuration(60_000, "en"), "1m");
  assert.equal(thoughtDuration(65_000, "en"), "1m 5s");
  // Units come from the platform's locale data ("12 Sek." in German).
  assert.match(thoughtDuration(12_000, "de"), /^12\s?\S+$/);
});
