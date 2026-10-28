import test from "node:test";
import assert from "node:assert/strict";
import {
  carriedReasoning,
  nearestReasoning,
  reasoningChoice,
  reasoningLevels,
  requestedReasoning,
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
  assert.equal(reasoningLabel("xhigh"), "Extra high");
});

test("a configured level is the one in use until another is chosen, and Model default overrides it", () => {
  const levels = ["low", "medium", "high"];
  // Nothing configured: unchanged, no choice is the model's default.
  assert.deepEqual(reasoningChoice("", levels), {
    steps: ["", "low", "medium", "high"],
    modelDefault: "",
    current: "",
    index: 0,
    unavailable: false,
    configured: false,
  });
  assert.equal(reasoningChoice("medium", levels).index, 2);
  // Configured high: with no choice the slider stands on High, marked as the default.
  const configured = reasoningChoice("", levels, "high");
  assert.deepEqual(configured.steps, ["default", "low", "medium", "high"]);
  assert.equal(configured.current, "high");
  assert.equal(configured.index, 3);
  assert.equal(configured.configured, true);
  // The Model default stop is a choice of its own there, and shows as such.
  const model = reasoningChoice("default", levels, "high");
  assert.equal(model.current, "");
  assert.equal(model.index, 0);
  assert.equal(model.unavailable, false);
  assert.equal(model.configured, false);
  assert.equal(reasoningLabel("default"), "Model default");
  assert.equal(reasoningChoice("low", levels, "high").configured, false);
  assert.equal(reasoningChoice("high", levels, "high").configured, true);
  // A configured level the model lacks stands where the server runs it.
  assert.equal(reasoningChoice("", ["low", "medium"], "max").current, "medium");
  assert.equal(reasoningChoice("xhigh", levels, "high").unavailable, true);
  // A model change keeps the model's default while a level is configured,
  // and drops a level the next model lacks.
  assert.equal(carriedReasoning("default", [], "high"), "default");
  assert.equal(carriedReasoning("default", levels), "");
  assert.equal(carriedReasoning("xhigh", levels, "high"), "");
  assert.equal(carriedReasoning("high", levels), "high");
});

test("the model's default is named only to a server that configures a level", () => {
  assert.equal(requestedReasoning("default", "high"), "default");
  assert.equal(requestedReasoning("default", undefined), undefined);
  assert.equal(requestedReasoning("default", null), undefined);
  assert.equal(requestedReasoning("low", undefined), "low");
  assert.equal(requestedReasoning("", "high"), undefined);
  assert.equal(requestedReasoning(undefined, "high"), undefined);
});

test("the nearest level is the one the server runs, the stronger on a tie", () => {
  assert.equal(nearestReasoning("medium", ["low", "medium", "high"]), "medium");
  assert.equal(nearestReasoning("none", ["low", "high"]), "low");
  assert.equal(nearestReasoning("medium", ["low", "high"]), "high");
  assert.equal(nearestReasoning("max", ["low", "high"]), "high");
  assert.equal(nearestReasoning("ultra", ["low", "max"]), "max");
  assert.equal(nearestReasoning("high", []), "");
});
