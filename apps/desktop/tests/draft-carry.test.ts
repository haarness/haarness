import { test } from "node:test";
import assert from "node:assert/strict";
import { carryDraft } from "../src/renderer/draft-carry";

test("a draft taken along is written at its destination before it leaves where it was, one write at a time", async () => {
  const steps: string[] = [];
  const part = (name: string) => ({
    write: async () => {
      steps.push("start " + name + " here");
      await new Promise((done) => setTimeout(done, 5));
      steps.push(name + " here");
    },
    clear: async () => {
      steps.push(name + " gone there");
    },
  });
  await carryDraft([part("text"), part("files"), part("chips")]);
  assert.deepEqual(steps, [
    "start text here",
    "text here",
    "start files here",
    "files here",
    "start chips here",
    "chips here",
    "text gone there",
    "files gone there",
    "chips gone there",
  ]);
});

test("a write that fails leaves the whole draft where it was", async () => {
  const source = { text: "typed", files: ["a.png"] };
  const destination: { text?: string; files?: string[] } = {};
  await assert.rejects(
    carryDraft([
      {
        write: async () => {
          destination.text = source.text;
        },
        clear: async () => {
          source.text = "";
        },
      },
      {
        write: async () => {
          throw new Error("disk full");
        },
        clear: async () => {
          source.files = [];
        },
      },
    ]),
    /disk full/,
  );
  assert.deepEqual(source, { text: "typed", files: ["a.png"] });
  assert.deepEqual(destination, { text: "typed" });
});
