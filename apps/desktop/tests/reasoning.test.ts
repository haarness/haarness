import test from "node:test";
import assert from "node:assert/strict";
import {
  reasoningLevels,
  validReasoning,
  reasoningLabel,
} from "../src/renderer/reasoning";

test("reasoning scales preserve advertised values and order across distinct models", () => {
  assert.deepEqual(reasoningLevels(["low", "high"]), ["low", "high"]);
  assert.deepEqual(
    reasoningLevels(["none", "minimal", "medium", "high", "xhigh"]),
    ["none", "minimal", "medium", "high", "xhigh"],
  );
  assert.deepEqual(
    reasoningLevels(["budget_auto", "budget_large", "budget_auto", ""]),
    ["budget_auto", "budget_large"],
  );
  assert.deepEqual(reasoningLevels(undefined), []);
});

test("switching models retains supported effort and drops unsupported values to model default", () => {
  assert.equal(validReasoning("high", ["low", "high"]), "high");
  assert.equal(validReasoning("xhigh", ["low", "high"]), "");
  assert.equal(validReasoning("high", []), "");
  assert.equal(validReasoning("none", ["none", "high"]), "none");
  assert.equal(reasoningLabel(""), "Model default");
  assert.equal(reasoningLabel("none"), "None");
  assert.equal(reasoningLabel("budget_large"), "Budget large");
});
