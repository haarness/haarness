import test from "node:test";
import assert from "node:assert/strict";
import { terminalSockets } from "../src/main/terminal-sockets";

const socket = (name: string) => ({ name, closed: false, close() { this.closed = true; } });

test("a late connect or detach of a replaced view leaves the shown view's socket alone", () => {
  const sockets = terminalSockets<ReturnType<typeof socket>>();
  const key = "1:local:/p:pty";
  // View A starts connecting, is replaced by B (tabs stepped quickly), and
  // A's connect finishes after B's started.
  sockets.begin(key, "A");
  sockets.detach(key, "A");
  sockets.begin(key, "B");
  const a = socket("a");
  assert.equal(sockets.accept(key, "A", 1, a), false);
  assert.equal(a.closed, true);
  const b = socket("b");
  assert.equal(sockets.accept(key, "B", 1, b), true);
  // A's view, gone, lets go once more: B keeps its socket.
  sockets.detach(key, "A");
  assert.equal(sockets.get(key)?.socket, b);
  assert.equal(b.closed, false);
  // B's socket ending by itself is reported, once.
  assert.equal(sockets.ended(key, b), true);
  assert.equal(sockets.ended(key, b), false);
  assert.equal(sockets.get(key), undefined);
});

test("a view that lets go while connecting gets no socket", () => {
  const sockets = terminalSockets<ReturnType<typeof socket>>();
  sockets.begin("k", "A");
  sockets.detach("k", "A");
  const a = socket("a");
  assert.equal(sockets.accept("k", "A", 1, a), false);
  assert.equal(a.closed, true);
  // A reconnect of the same view replaces its old socket.
  sockets.begin("k", "B");
  const first = socket("first");
  sockets.accept("k", "B", 1, first);
  sockets.begin("k", "B");
  assert.equal(first.closed, true);
  // A socket replaced that way is not reported as ended.
  assert.equal(sockets.ended("k", first), false);
});

test("a closed window's sockets close, and other windows keep theirs", () => {
  const sockets = terminalSockets<ReturnType<typeof socket>>();
  const one = socket("one"), two = socket("two");
  sockets.begin("1:c:p", "A");
  sockets.accept("1:c:p", "A", 1, one);
  sockets.begin("2:c:p", "B");
  sockets.accept("2:c:p", "B", 2, two);
  sockets.close(1);
  assert.equal(one.closed, true);
  assert.equal(two.closed, false);
  assert.equal(sockets.get("1:c:p"), undefined);
  sockets.closeAll();
  assert.equal(two.closed, true);
});
