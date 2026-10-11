import test from "node:test";
import assert from "node:assert/strict";
import type { WorkflowRun } from "@haarness/sdk";
import {
  commandNote,
  followRun,
  latestRun,
} from "../src/renderer/command-result";

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
    run: "0190-run",
  });
  assert.equal(
    commandNote("/workflow cancel 0190-run", {
      kind: "workflow_run",
      run: { ...run, status: "failed", error: "agent crashed " },
    })?.text,
    "Workflow fixture failed.\nagent crashed\nRun ID: 0190-run",
  );
});

test("a workflow command's answer follows its run to the end", () => {
  const run = (status: WorkflowRun["status"], updated: string) =>
    ({
      id: "0190-run",
      workflowName: "fixture",
      status,
      time: { created: "2026-10-11T03:00:00Z", updated },
    }) as WorkflowRun;
  const running = run("running", "2026-10-11T03:00:01Z");
  const completed = run("completed", "2026-10-11T03:00:02Z");
  const note = commandNote("/workflow run fixture", {
    kind: "workflow_run",
    run: running,
  })!;
  assert.deepEqual(followRun(note, completed), {
    title: "/workflow run fixture",
    text: "Workflow fixture finished.\nRun ID: 0190-run",
    run: "0190-run",
  });
  // A quick workflow can finish before the command's answer arrives.
  assert.equal(latestRun(running, completed), completed);
  assert.equal(latestRun(completed, running), completed);
  assert.equal(latestRun(running, undefined), running);
});

test("a command that starts a turn answers in the transcript instead", () => {
  assert.equal(
    commandNote("/review", { kind: "turn", turn: {} as never }),
    null,
  );
});
