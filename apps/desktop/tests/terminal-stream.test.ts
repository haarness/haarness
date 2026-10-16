import test from "node:test";
import assert from "node:assert/strict";
import { reconnectDelay, replayGate, writeChunks, WRITE_LIMIT } from "../src/renderer/terminal-stream";

/** A stand-in for xterm: written data is parsed later, in order, and
 * `write("", done)` calls back once everything before it is parsed. */
function fakeTerminal() {
  const queue: (() => void)[] = [];
  return {
    flush: (done: () => void) => void queue.push(done),
    parse: () => queue.splice(0).forEach((done) => done()),
  };
}

test("answers to queries in replayed output never reach the shell; live ones do", () => {
  const xterm = fakeTerminal();
  const gate = replayGate(xterm.flush);
  // Shut from the start: the server replays before anything live.
  assert.equal(gate.open(), false);
  gate.replay();
  // The replay ends with a cursor frame, but xterm has not parsed it yet:
  // an answer to a replayed `ESC[c` produced now is still dropped.
  gate.cursor();
  assert.equal(gate.open(), false);
  // Every live chunk is followed by another cursor frame; that asks for no
  // second flush.
  gate.cursor();
  xterm.parse();
  assert.equal(gate.open(), true);
  // A reconnect replays the missed range: shut again until it is parsed.
  gate.replay();
  assert.equal(gate.open(), false);
  gate.cursor();
  xterm.parse();
  assert.equal(gate.open(), true);
});

test("a flush from an earlier replay does not open the gate during a newer one", () => {
  const xterm = fakeTerminal();
  const gate = replayGate(xterm.flush);
  gate.replay();
  gate.cursor();
  // The socket dropped and the next replay began before xterm caught up.
  gate.replay();
  xterm.parse();
  assert.equal(gate.open(), false);
  gate.cursor();
  xterm.parse();
  assert.equal(gate.open(), true);
});

test("a long paste reaches the PTY in pieces the desktop accepts, in order", () => {
  const paste = new TextEncoder().encode("é".repeat(40_000));
  const chunks = writeChunks(paste);
  assert.equal(chunks.length, 3);
  assert.ok(chunks.every((chunk) => chunk.length <= WRITE_LIMIT));
  assert.deepEqual(
    Buffer.concat(chunks.map((chunk) => Buffer.from(chunk))),
    Buffer.from(paste),
  );
  assert.deepEqual(writeChunks(new Uint8Array()), []);
});

test("a terminal reconnects quickly at first and at most every five seconds", () => {
  assert.deepEqual(
    [0, 1, 2, 3, 4, 5, 6, 40].map(reconnectDelay),
    [250, 500, 1000, 2000, 4000, 5000, 5000, 5000],
  );
});
