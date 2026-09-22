import { test } from "node:test";
import assert from "node:assert/strict";
import {
  orderedIds,
  moveInOrder,
  readOrdering,
} from "../src/renderer/ordering";

test("saved positions survive activity, rename, discovery and temporary removal", () => {
  const saved = ["b", "a", "c"];
  assert.deepEqual(orderedIds(["c", "b", "a"], saved), saved);
  assert.deepEqual(orderedIds(["d", "c", "a", "b"], saved, "start"), [
    "d",
    "b",
    "a",
    "c",
  ]);
  assert.deepEqual(orderedIds(["c", "b"], saved), ["b", "c"]);
  assert.deepEqual(orderedIds(["a", "b", "c", "d"], saved), [
    "b",
    "a",
    "c",
    "d",
  ]);
});
test("reordering filtered rows keeps hidden positions and supports either end", () => {
  assert.deepEqual(
    moveInOrder(["a", "hidden", "b", "c"], ["a", "b", "c"], "c", 0),
    ["c", "hidden", "a", "b"],
  );
  assert.deepEqual(moveInOrder(["a", "b", "c"], ["a", "b", "c"], "a", 9), [
    "b",
    "c",
    "a",
  ]);
  assert.deepEqual(moveInOrder(["a"], ["a"], "missing", 0), ["a"]);
});
test("versioned ordering data validates and separates collection scopes", () => {
  assert.deepEqual(
    readOrdering({ version: 9, lists: { "list:projects": ["a"] } }),
    {},
  );
  assert.deepEqual(
    readOrdering({
      version: 1,
      lists: {
        "list:projects": ["b", "b", "a"],
        "list:sessions:workspace-a": ["1", "2"],
        "list:sessions:workspace-b": ["2", "1"],
        malformed: ["a"],
        "list:bad": [1],
      },
    }),
    {
      "list:projects": ["b", "a"],
      "list:sessions:workspace-a": ["1", "2"],
      "list:sessions:workspace-b": ["2", "1"],
    },
  );
});
