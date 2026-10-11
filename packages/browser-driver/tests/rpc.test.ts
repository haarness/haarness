import test from "node:test";
import assert from "node:assert/strict";
import {
  Browser,
  decodeCommand,
  decodeHostCommand,
  decodeHostOutcome,
  encodeHostOutcome,
  encodeOutcome,
} from "../src/rpc";

const id = "file_00000000-0000-4000-8000-000000000001";
const tabID = "tab_00000000-0000-4000-8000-000000000002";
const upload = { type: "files.upload", tabID, ref: "e1", paths: ["a.txt"] };

test("the server wire carries file bytes only", () => {
  const command = decodeCommand({
    action: upload,
    files: [{ id, name: "a.txt", mime: "text/plain", data: "aGk=" }],
  });
  assert.deepEqual([...(command.files[0] as Browser.File).data], [104, 105]);
  // A staged path never reaches Desktop or the server.
  assert.throws(() =>
    decodeCommand({
      action: upload,
      files: [{ id, name: "a.txt", mime: "text/plain", bytes: 2, path: "/tmp/a.txt" }],
    }),
  );
  const encoded = encodeOutcome({
    type: "success",
    result: {
      value: null,
      files: [{ id: Browser.FileID.make(id), name: "a.txt", mime: "text/plain", data: new Uint8Array([104, 105]) }],
    },
  });
  assert.deepEqual(encoded, {
    type: "success",
    result: { value: null, files: [{ id, name: "a.txt", mime: "text/plain", data: "aGk=" }] },
  });
});

test("the bridge and the extension exchange bytes or staged paths", () => {
  const command = decodeHostCommand({
    action: upload,
    files: [
      { id, name: "a.txt", mime: "text/plain", data: "aGk=" },
      { id, name: "b.txt", mime: "text/plain", bytes: 3, path: "/staged/b.txt" },
    ],
    target: { resources: ["https://example.test"], key: "k" },
  });
  assert.ok("data" in command.files[0]);
  assert.deepEqual(command.files[1], { id, name: "b.txt", mime: "text/plain", bytes: 3, path: "/staged/b.txt" });
  assert.throws(() =>
    decodeHostCommand({ action: upload, files: [{ id, name: "c", mime: "x", path: "/p" }] }),
  );
  const outcome = {
    type: "success" as const,
    result: {
      value: { tab: null },
      files: [
        { id: Browser.FileID.make(id), name: "report.pdf", mime: "application/pdf", bytes: 9, path: "/Downloads/report.pdf" },
        { id: Browser.FileID.make(id), name: "shot.png", mime: "image/png", data: new Uint8Array([1, 2, 3]) },
      ],
    },
  };
  const json = encodeHostOutcome(outcome);
  assert.deepEqual(json, {
    type: "success",
    result: {
      value: { tab: null },
      files: [
        { id, name: "report.pdf", mime: "application/pdf", bytes: 9, path: "/Downloads/report.pdf" },
        { id, name: "shot.png", mime: "image/png", data: "AQID" },
      ],
    },
  });
  assert.deepEqual(decodeHostOutcome(JSON.parse(JSON.stringify(json))), outcome);
  assert.deepEqual(encodeHostOutcome({ type: "failure", code: "paused", message: "Paused" }), {
    type: "failure",
    code: "paused",
    message: "Paused",
  });
});
