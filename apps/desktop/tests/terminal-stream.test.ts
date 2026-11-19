import test from "node:test";
import assert from "node:assert/strict";
import {
  clearPoints,
  DISCONNECTED_AFTER,
  RECONNECTING_AFTER,
  inputQueue,
  linkWatch,
  RELAYED_RECONNECTING_AFTER,
  reconnectDelay,
  reconnects,
  restoredLine,
  scrollPlaces,
  shownOutput,
  terminalStream,
  topText,
  writeChunks,
  WRITE_LIMIT,
  type Link,
} from "../src/renderer/terminal-stream";

/**
 * A stand-in for xterm: written bytes are parsed later, in order, and a
 * write's callback runs right after its bytes are parsed. Parsing a query
 * (`?`) makes it answer, and the answer reaches the shell only while the
 * stream lets it. A clear notes how many bytes were parsed then; while
 * `alternate` is set a full-screen program has the screen, and it does
 * nothing.
 */
function fakeTerminal(shown: Map<string, number>, key = "pty") {
  const queue: { bytes: Uint8Array; done?: () => void }[] = [];
  const answered: number[] = [];
  const clears: number[] = [];
  const screen = { alternate: false };
  const stream = terminalStream({
    write: (bytes, done) => void queue.push({ bytes, done }),
    shown: () => shown.get(key) ?? 0,
    show: (end) => shown.set(key, Math.max(end, shown.get(key) ?? 0)),
    clearedAt: () => clearPoints.get(key),
    cleared: (end) => clearPoints.set(key, end),
    clear: () => {
      if (screen.alternate) return false;
      clears.push(parsed);
      return true;
    },
  });
  let parsed = 0;
  return {
    stream,
    answered,
    clears,
    screen,
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

test("a clear waits for the output before it, and a view shown again clears at the same point", () => {
  const shown = new Map<string, number>();
  const first = fakeTerminal(shown, "c:cleared");
  first.stream.replay(0);
  first.stream.bytes(text("seq\n1\n2\n$ "));
  // ⌘K before xterm has drawn that output: it is cleared too.
  first.stream.clear();
  first.stream.bytes(text("ls\n"));
  first.parse();
  assert.deepEqual(first.clears, [10]);
  assert.equal(clearPoints.get("c:cleared"), 10);
  // Shown again, the replay holds all of it: the view clears after the same
  // ten bytes, wherever the chunks break, and shows what came after.
  for (const chunks of [["seq\n1\n2\n$ ls\n"], ["seq\n1\n2", "\n$ ", "ls\n"], ["s", "eq\n1\n2\n$ l", "s\n"]]) {
    const again = fakeTerminal(shown, "c:cleared");
    again.stream.replay(0);
    for (const chunk of chunks) again.stream.bytes(text(chunk));
    again.parse();
    assert.deepEqual(again.clears, [10], chunks.join("|"));
  }
  // A reconnect from the view's cursor replays only newer output: no clear.
  const reconnected = fakeTerminal(shown, "c:cleared");
  reconnected.stream.replay(13);
  reconnected.stream.bytes(text("x"));
  reconnected.parse();
  assert.deepEqual(reconnected.clears, []);
});

test("a clear point only moves on, and goes once the server no longer has the output before it", () => {
  const view = fakeTerminal(new Map(), "c:moved");
  view.stream.replay(0);
  view.stream.bytes(text("0123456789"));
  view.stream.clear();
  view.parse();
  assert.equal(clearPoints.get("c:moved"), 10);
  // ⌘K in a view shown again, before its replay arrived, keeps the point.
  const early = fakeTerminal(new Map(), "c:moved");
  early.stream.clear();
  early.parse();
  assert.equal(clearPoints.get("c:moved"), 10);
  // The server dropped output up to and past the point: nothing to clear.
  clearPoints.kept("c:moved", 4);
  assert.equal(clearPoints.get("c:moved"), 10);
  clearPoints.kept("c:moved", 10);
  assert.equal(clearPoints.get("c:moved"), undefined);
  // A replay that starts after a point clears nothing.
  clearPoints.set("c:moved", 10);
  const later = fakeTerminal(new Map(), "c:moved");
  later.stream.replay(12);
  later.stream.bytes(text("abc"));
  later.parse();
  assert.deepEqual(later.clears, []);
  // Terminals that are gone take their points with them.
  shownOutput.keep("c:", []);
  assert.equal(clearPoints.get("c:moved"), undefined);
});

test("a clear while a full-screen program has the alternate screen does nothing, and leaves no point", () => {
  const view = fakeTerminal(new Map(), "c:less");
  view.stream.replay(0);
  view.stream.bytes(text("\u001b[?1049hpage"));
  view.screen.alternate = true;
  view.stream.clear();
  view.parse();
  assert.deepEqual(view.clears, []);
  assert.equal(clearPoints.get("c:less"), undefined);
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

/** A clock the test moves: timers run when it passes their time. */
function clock() {
  let now = 0;
  let timers: { at: number; run: () => void; id: number }[] = [];
  let next = 0;
  return {
    schedule: (run: () => void, ms: number) => {
      const id = ++next;
      timers.push({ at: now + ms, run, id });
      return id as unknown as ReturnType<typeof setTimeout>;
    },
    cancel: (timer: ReturnType<typeof setTimeout>) => {
      timers = timers.filter((item) => item.id !== (timer as unknown as number));
    },
    advance(ms: number) {
      now += ms;
      for (const item of timers.filter((item) => item.at <= now).sort((a, b) => a.at - b.at)) {
        timers = timers.filter((other) => other !== item);
        item.run();
      }
    },
  };
}

test("another computer's terminal says nothing while Haarness Cloud renews a sign-in", () => {
  const time = clock();
  const states: Link[] = [];
  const watch = linkWatch({ onState: (link) => states.push(link), quietMs: RELAYED_RECONNECTING_AFTER, ...time });
  watch.lost();
  time.advance(RELAYED_RECONNECTING_AFTER - 1);
  watch.connected();
  assert.deepEqual(states, []);
  watch.lost();
  time.advance(RELAYED_RECONNECTING_AFTER);
  assert.deepEqual(states, ["reconnecting"]);
  assert.ok(RELAYED_RECONNECTING_AFTER > RECONNECTING_AFTER && RELAYED_RECONNECTING_AFTER < DISCONNECTED_AFTER);
});

test("a dropped terminal says nothing for a quick reconnect, then that it reconnects, then that it cannot reach its shell", () => {
  const time = clock();
  const states: Link[] = [];
  const watch = linkWatch({ onState: (link) => states.push(link), ...time });
  // The server lets go of a view behind a burst and it connects at once.
  watch.lost();
  time.advance(RECONNECTING_AFTER - 1);
  watch.connected();
  time.advance(DISCONNECTED_AFTER);
  assert.deepEqual(states, []);
  // The server is gone: a second on, it reconnects; later, it gives up saying so.
  watch.lost();
  time.advance(RECONNECTING_AFTER);
  watch.lost();
  assert.deepEqual(states, ["reconnecting"]);
  time.advance(DISCONNECTED_AFTER - RECONNECTING_AFTER);
  assert.deepEqual(states, ["reconnecting", "disconnected"]);
  // Retry says so at once and waits the whole time again.
  watch.retry();
  time.advance(DISCONNECTED_AFTER - 1);
  assert.deepEqual(states, ["reconnecting", "disconnected", "reconnecting"]);
  time.advance(1);
  assert.deepEqual(states, ["reconnecting", "disconnected", "reconnecting", "disconnected"]);
  // The background reconnect that succeeds clears it.
  watch.connected();
  assert.equal(states.at(-1), "connected");
  watch.dispose();
});

test("a terminal shown again opens where it was scrolled to, while that line holds the same text", () => {
  const lines = (from: number) => (index: number) => `row ${index + from}`;
  const saved = { start: 0, end: 9000, line: 120, cols: 80, rows: 24, text: topText(lines(0), 120) };
  assert.equal(saved.text, "row 120\nrow 121");
  const text = (from: number) => (line: number) => topText(lines(from), line);
  const now = { start: 0, end: 9000, cols: 80, rows: 24, trimmed: false, text: text(0) };
  // The same output at the same size, even past xterm's scrollback.
  assert.equal(restoredLine(saved, now), 120);
  assert.equal(restoredLine(saved, { ...now, trimmed: true }), 120);
  // More output while hidden: the lines stay put until xterm drops some.
  assert.equal(restoredLine(saved, { ...now, end: 12000 }), 120);
  assert.equal(restoredLine(saved, { ...now, end: 12000, trimmed: true }), undefined);
  // Another height: the same, since xterm keeps rows plus its scrollback.
  assert.equal(restoredLine(saved, { ...now, rows: 30 }), 120);
  assert.equal(restoredLine(saved, { ...now, rows: 30, trimmed: true }), undefined);
  // Another width wraps other lines; a replay that starts later holds others.
  assert.equal(restoredLine(saved, { ...now, cols: 100 }), undefined);
  assert.equal(restoredLine(saved, { ...now, start: 4096 }), undefined);
  assert.equal(restoredLine(undefined, now), undefined);
  // The scrollback was erased while hidden and refilled: other text there.
  assert.equal(restoredLine(saved, { ...now, end: 12000, text: text(500) }), undefined);
  // Lines xterm no longer has read as empty.
  assert.equal(topText(() => undefined, 3), "\n");

  scrollPlaces.save("c:a", saved);
  assert.deepEqual(scrollPlaces.take("c:a"), saved);
  // Taken once: a reconnect's replay does not scroll the view back.
  assert.equal(scrollPlaces.take("c:a"), undefined);
  // At the bottom, nothing is kept.
  scrollPlaces.save("c:a", saved);
  scrollPlaces.save("c:a", undefined);
  assert.equal(scrollPlaces.take("c:a"), undefined);
  // A terminal that is gone is forgotten.
  scrollPlaces.save("c:b", saved);
  shownOutput.keep("c:", ["a"]);
  assert.equal(scrollPlaces.take("c:b"), undefined);
});

test("a terminal never has two reconnect attempts under way, and Retry during one backs off from the start", async () => {
  const time = clock();
  const settled = () => new Promise((resolve) => setImmediate(resolve));
  const attempts: ((done: boolean) => void)[] = [];
  const loop = reconnects({
    attempt: () => new Promise<boolean>((resolve) => attempts.push(resolve)),
    ...time,
  });
  loop.now();
  time.advance(0);
  assert.equal(attempts.length, 1);
  // Retry, and a drop reported meanwhile, while the attempt is out.
  loop.now();
  loop.after();
  time.advance(10_000);
  assert.equal(attempts.length, 1);
  // It fails: the next comes after the shortest delay, then longer ones.
  attempts[0](false);
  await settled();
  time.advance(249);
  assert.equal(attempts.length, 1);
  time.advance(1);
  assert.equal(attempts.length, 2);
  attempts[1](false);
  await settled();
  time.advance(499);
  assert.equal(attempts.length, 2);
  time.advance(1);
  assert.equal(attempts.length, 3);
  // Retry while waiting goes at once.
  attempts[2](false);
  await settled();
  loop.now();
  time.advance(0);
  assert.equal(attempts.length, 4);
  // Connected: nothing more until the connection ends again.
  attempts[3](true);
  await settled();
  time.advance(10_000);
  assert.equal(attempts.length, 4);
  loop.after();
  loop.dispose();
  time.advance(10_000);
  assert.equal(attempts.length, 4);
});
