import assert from "node:assert/strict";
import test from "node:test";
import { createReassembler, isChunk, type Chunk } from "../src/core/chunks";

/** Splits a message the way the bridge does: base64 of at most `size` bytes of its UTF-8 JSON per chunk. */
function split(message: unknown, size: number, id = "chunk-1"): Chunk[] {
  const bytes = Buffer.from(JSON.stringify(message), "utf8");
  const count = Math.ceil(bytes.length / size);
  return Array.from({ length: count }, (_, index) => ({
    type: "chunk",
    id,
    index,
    count,
    data: bytes.subarray(index * size, (index + 1) * size).toString("base64"),
  }));
}

test("reassembles a message whose cuts fall inside multibyte characters", () => {
  const message = { type: "from-server", server: "s", message: { title: "Café ☕ 東京 — \"quoted\" \\ end", pad: "ü".repeat(60) } };
  for (const size of [1, 2, 3, 7, 64]) {
    const reassembler = createReassembler();
    const chunks = split(message, size);
    assert.ok(chunks.every(isChunk));
    const results = chunks.map((chunk) => reassembler.push(chunk));
    assert.deepEqual(results.at(-1), message, `size ${size}`);
    assert.ok(results.slice(0, -1).every((result) => result === undefined));
  }
});

test("a gap, a foreign id or a bad index drops the message", () => {
  const chunks = split({ value: "x".repeat(100) }, 10);
  const reassembler = createReassembler();
  reassembler.push(chunks[0]);
  assert.throws(() => reassembler.push(chunks[2]), /out of sequence/);
  // A new message after an abandoned one starts cleanly.
  const next = split({ ok: true }, 4, "chunk-2");
  assert.deepEqual(next.map((chunk) => reassembler.push(chunk)).at(-1), { ok: true });
  assert.throws(() => reassembler.push({ ...chunks[0], index: 5, count: 2 }), /Malformed/);
  assert.equal(isChunk({ type: "chunk", id: "x", index: "0", count: 1, data: "" }), false);
});
