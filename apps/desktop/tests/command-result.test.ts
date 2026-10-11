import test from "node:test";
import assert from "node:assert/strict";
import type { WorkflowRun } from "@haarness/sdk";
import { commandNote } from "../src/renderer/command-result";

test("a command that answers with text shows that text under the command", () => {
  assert.deepEqual(
    commandNote("/workflow", {
      kind: "text",
      text: "Workflows (1)\n/fixture · Desktop verification\n",
    }),
    { title: "/workflow", text: "Workflows (1)\n/fixture · Desktop verification" },
  );
  assert.deepEqual(
    commandNote(" /goal status ", { kind: "text", text: "Goal paused" }),
    { title: "/goal status", text: "Goal paused" },
  );
  assert.equal(commandNote("/goal", { kind: "text", text: "  " }), null);
});

test("a workflow command names the run, its state and the ID the other commands take", () => {
  const run = {
    id: "0190-run",
    workflowName: "fixture",
    status: "running",
  } as WorkflowRun;
  assert.deepEqual(commandNote("/workflow run fixture", { kind: "workflow_run", run }), {
    title: "/workflow run fixture",
    text: "Workflow fixture is running.\nRun ID: 0190-run",
  });
  assert.equal(
    commandNote("/workflow cancel 0190-run", {
      kind: "workflow_run",
      run: { ...run, status: "failed", error: "agent crashed " },
    })?.text,
    "Workflow fixture failed.\nagent crashed\nRun ID: 0190-run",
  );
});

test("a command that starts a turn answers in the transcript instead", () => {
  assert.equal(
    commandNote("/review", { kind: "turn", turn: {} as never }),
    null,
  );
});
