import test from "node:test";
import assert from "node:assert/strict";
import { inputQueue, reconnectDelay, shownOutput, terminalStream, writeChunks, WRITE_LIMIT } from "../src/renderer/terminal-stream";

/**
 * A stand-in for xterm: written bytes are parsed later, in order, and a
 * write's callback runs right after its bytes are parsed. Parsing a query
 * (`?`) makes it answer, and the answer reaches the shell only while the
 * stream lets it.
 */
function fakeTerminal(shown: Map<string, number>, key = "pty") {
  const queue: { bytes: Uint8Array; done?: () => void }[] = [];
  const answered: number[] = [];
  const stream = terminalStream({
    write: (bytes, done) => void queue.push({ bytes, done }),
    shown: () => shown.get(key) ?? 0,
    show: (end) => shown.set(key, Math.max(end, shown.get(key) ?? 0)),
  });
  let parsed = 0;
  return {
    stream,
    answered,
    parse() {
      for (const { bytes, done } of queue.splice(0)) {
        for (const byte of bytes) {
          if (byte === 63 && stream.open()) answered.push(parsed);
          parsed++;
        }
        done?.();
      }
    },
  };
}
const text = (value: string) => new TextEncoder().encode(value);

test("a fresh terminal answers the queries in its first output", () => {
  const shown = new Map<string, number>();
  const view = fakeTerminal(shown);
  // A new shell (fish) asks for the device attributes and waits.
  view.stream.replay(0);
  view.stream.bytes(text("prompt ?"));
  view.stream.cursor(8);
  view.parse();
  assert.deepEqual(view.answered, [7]);
});

test("a terminal shown again leaves the queries it already showed unanswered, and answers new ones", () => {
  const shown = new Map<string, number>();
  const first = fakeTerminal(shown);
  first.stream.replay(0);
  first.stream.bytes(text("a ? b "));
  first.parse();
  assert.deepEqual(first.answered, [2]);
  // Shown again: the replay holds the same six bytes and two new ones
  // (written while the panel was hidden), in one chunk.
  const again = fakeTerminal(shown);
  again.stream.replay(0);
  again.stream.bytes(text("a ? b ??"));
  again.parse();
  assert.deepEqual(again.answered, [6, 7]);
  // Live output after the replay is answered too.
  again.stream.bytes(text("?"));
  again.parse();
  assert.deepEqual(again.answered, [6, 7, 8]);
});

test("after a reconnect only the output missed while it was down is answered", () => {
  const shown = new Map<string, number>([["pty", 10]]);
  const view = fakeTerminal(shown);
  // The view reconnects from its cursor (10); the server replays from 10.
  view.stream.replay(10);
  view.stream.bytes(text("?x"));
  view.parse();
  assert.equal(view.answered.length, 1);
  // A replay the server starts earlier (the view asked from 0) shuts the
  // answers for bytes 0–9, then opens again.
  const other = fakeTerminal(new Map([["pty", 10]]));
  other.stream.replay(0);
  other.stream.bytes(text("?????????" + "?" + "??"));
  other.parse();
  assert.deepEqual(other.answered, [10, 11]);
});

test("the view's own bytes before a truncated replay are never answered, and the replay's new output is", () => {
  const view = fakeTerminal(new Map());
  // A reset, the notice and the restored modes, then output never shown.
  view.stream.replay(5);
  view.stream.quiet(text("\u001bc?notice?\u001b[?1h"));
  view.stream.bytes(text("x?"));
  view.parse();
  assert.deepEqual(view.answered, [16]);
});

test("input typed while the connection is down is kept in order, up to a limit", () => {
  const queue = inputQueue(4);
  queue.add(2, text("b"));
  queue.add(1, text("a"));
  queue.add(3, text("c"));
  assert.deepEqual(queue.take().map((bytes) => new TextDecoder().decode(bytes)), ["a", "b", "c"]);
  assert.deepEqual(queue.take(), []);
  // Past the limit the oldest goes, so the latest key (Ctrl+C) still lands.
  queue.add(1, text("yes"));
  queue.add(2, text("\u0003"));
  queue.add(3, text("ls"));
  assert.deepEqual(queue.take().map((bytes) => new TextDecoder().decode(bytes)), ["\u0003", "ls"]);
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

test("a terminal this window did not start counts its first replay as shown", () => {
  shownOutput.started("c:new");
  shownOutput.adopt("c:new", 500);
  assert.equal(shownOutput.get("c:new"), 0);
  shownOutput.adopt("c:old", 500);
  assert.equal(shownOutput.get("c:old"), 500);
  // Later replays leave what was counted as it is.
  shownOutput.adopt("c:old", 900);
  assert.equal(shownOutput.get("c:old"), 500);
  shownOutput.keep("c:", ["new"]);
  assert.equal(shownOutput.get("c:old"), 0);
});
