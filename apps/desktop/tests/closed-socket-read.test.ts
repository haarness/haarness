import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { EventEmitter } from "node:events";
import { getSystemErrorName } from "node:util";
import { closedSocketRead, quietClosedSocketReads } from "../src/main/closed-socket-read";

/** An error with the fields Node gives its own. */
function nodeError(Type: ErrorConstructor, code: string, message: string, frames: string[]) {
  const error = Object.assign(new Type(message), { code });
  error.stack = `${error.name} [${code}]: ${message}\n${frames.map((frame) => "    at " + frame).join("\n")}`;
  return error;
}
const message = 'The value of "err" is out of range. It must be a negative integer. Received 216';
/** As Haarness Desktop's dialog showed it, from Electron 44 (Node 24.20). */
const reported = nodeError(RangeError, "ERR_OUT_OF_RANGE", message, [
  "Object.getSystemErrorName (node:util:455:11)",
  "new ErrnoException (node:internal/errors:734:23)",
  "TCP.onStreamRead (node:internal/stream_base_commons:216:20)",
]);

/** What this Node throws for real: a socket marked destroyed while its
 * handle still reads (as code ending it inside one of its reads leaves it),
 * then 216 bytes arrive. Run apart, as the test runner fails a test on any
 * uncaught exception. */
function thrownHere() {
  const script = `
    const net = require("node:net");
    process.on("uncaughtException", (error) => {
      process.stdout.write(JSON.stringify({ range: error instanceof RangeError, code: error.code, message: error.message, stack: error.stack }));
      process.exit(0);
    });
    const server = net.createServer((peer) => setTimeout(() => peer.write(Buffer.alloc(216, 120)), 20));
    server.listen(0, "127.0.0.1", () => {
      const socket = net.connect(server.address().port, "127.0.0.1", () => { socket.destroyed = true; });
      socket.on("data", () => {});
    });
    setTimeout(() => process.exit(1), 10000);
  `;
  const seen = JSON.parse(execFileSync(process.execPath, ["-e", script], { encoding: "utf8", timeout: 20000 }));
  assert.equal(seen.range, true);
  return Object.assign(new RangeError(seen.message), { code: seen.code, stack: seen.stack });
}

test("Node's error for a read that reached a closed socket is recognised, as reported and as this Node throws it", () => {
  assert.equal(closedSocketRead(reported), true);
  const real = thrownHere();
  assert.match(real.stack!, /at TCP\.onStreamRead \(node:internal\/stream_base_commons:/);
  assert.equal(closedSocketRead(real), true);
  // A TLS socket's or a pipe's read goes through the same function.
  for (const owner of ["TLSWrap", "Pipe"])
    assert.equal(
      closedSocketRead(
        nodeError(RangeError, "ERR_OUT_OF_RANGE", message.replace("216", "65536"), [
          "Object.getSystemErrorName (node:util:455:11)",
          "new ErrnoException (node:internal/errors:734:23)",
          `${owner}.onStreamRead (node:internal/stream_base_commons:216:20)`,
        ]),
      ),
      true,
    );
});

test("every other error is not, however close", () => {
  const frames = [
    "Object.getSystemErrorName (node:util:455:11)",
    "new ErrnoException (node:internal/errors:734:23)",
    "TCP.onStreamRead (node:internal/stream_base_commons:216:20)",
  ];
  // The same check failing in code that asked for a name itself.
  let own: unknown;
  try {
    getSystemErrorName(216);
  } catch (error) {
    own = error;
  }
  assert.equal((own as { code?: string }).code, "ERR_OUT_OF_RANGE");
  assert.equal(closedSocketRead(own), false);
  for (const other of [
    // Thrown by a 'data' listener that read past a buffer, under a read.
    nodeError(RangeError, "ERR_OUT_OF_RANGE", 'The value of "offset" is out of range. It must be >= 0 and <= 4. Received 8', [
      "boundsError (node:internal/buffer:88:9)",
      "Buffer.readUInt32BE (node:internal/buffer:225:5)",
      "Socket.<anonymous> (/app/out/main/index.js:10:20)",
      "Socket.emit (node:events:518:28)",
      "addChunk (node:internal/streams/readable:561:12)",
      "readableAddChunkPushByteMode (node:internal/streams/readable:512:3)",
      "Readable.push (node:internal/streams/readable:392:5)",
      "TCP.onStreamRead (node:internal/stream_base_commons:189:23)",
    ]),
    // Called from JavaScript rather than by a read.
    nodeError(RangeError, "ERR_OUT_OF_RANGE", message, [...frames, "Socket.read (/app/out/main/index.js:1:1)"]),
    // Frames cut off, or none.
    nodeError(RangeError, "ERR_OUT_OF_RANGE", message, frames.slice(0, 2)),
    nodeError(RangeError, "ERR_OUT_OF_RANGE", message, []),
    // Another code, another message, another class.
    nodeError(RangeError, "ERR_INVALID_ARG_TYPE", message, frames),
    nodeError(RangeError, "ERR_OUT_OF_RANGE", message.replace("Received 216", "Received 0"), frames),
    nodeError(RangeError, "ERR_OUT_OF_RANGE", message + " (and more)", frames),
    nodeError(TypeError as ErrorConstructor, "ERR_OUT_OF_RANGE", message, frames),
    // The frames of user code that happens to share names.
    nodeError(RangeError, "ERR_OUT_OF_RANGE", message, [
      "getSystemErrorName (/app/out/main/index.js:1:1)",
      "new ErrnoException (/app/out/main/index.js:2:1)",
      "onStreamRead (/app/out/main/index.js:3:1)",
    ]),
  ])
    assert.equal(closedSocketRead(other), false, other.stack);
  // Not errors at all, or a look-alike that is not one.
  for (const value of [undefined, null, message, 216, { name: "RangeError", code: "ERR_OUT_OF_RANGE", message, stack: reported.stack }])
    assert.equal(closedSocketRead(value), false);
});

/** A process with Electron's own listener: it shows its dialog only while it
 * is the one listener, as `lib/browser/init.ts` does. */
function electronProcess() {
  const target = new EventEmitter();
  const dialogs: string[] = [];
  target.on("uncaughtException", (error: Error) => {
    if (target.listenerCount("uncaughtException") > 1) return;
    dialogs.push("Uncaught Exception:\n" + (error.stack ? error.stack : `${error.name}: ${error.message}`));
  });
  return { target, dialogs };
}

test("a closed socket's read is noted once and shows no dialog; every other error still shows Electron's", () => {
  const { target, dialogs } = electronProcess();
  const recorded: Error[] = [];
  quietClosedSocketReads(target, (error) => recorded.push(error));
  assert.equal(target.listenerCount("uncaughtException"), 1);
  target.emit("uncaughtException", reported, "uncaughtException");
  target.emit("uncaughtException", reported, "uncaughtException");
  assert.deepEqual(dialogs, []);
  assert.deepEqual(recorded, [reported]);
  const other = new Error("Something else broke");
  target.emit("uncaughtException", other, "uncaughtException");
  assert.deepEqual(dialogs, ["Uncaught Exception:\n" + other.stack]);
  assert.deepEqual(recorded, [reported], "nothing else is noted");
  // A listener added later keeps Electron quiet, as it always has.
  target.on("uncaughtException", () => {});
  target.emit("uncaughtException", other, "uncaughtException");
  assert.equal(dialogs.length, 1);
});

test("the listener that was there hears every other error as before", () => {
  const target = new EventEmitter();
  const heard: unknown[][] = [];
  target.on("uncaughtException", function (this: unknown, ...args: unknown[]) {
    heard.push([this, ...args]);
  });
  quietClosedSocketReads(target, () => {});
  const other = new RangeError("Another range");
  target.emit("uncaughtException", other, "unhandledRejection");
  target.emit("uncaughtException", reported, "uncaughtException");
  assert.deepEqual(heard, [[target, other, "unhandledRejection"]]);
});

test("anything but Electron's one listener is left as it is, with nothing installed", () => {
  const recorded: Error[] = [];
  // No listener (outside Electron).
  const bare = new EventEmitter();
  quietClosedSocketReads(bare, (error) => recorded.push(error));
  assert.equal(bare.listenerCount("uncaughtException"), 0);
  // Two listeners: both keep hearing everything, this error too.
  const two = new EventEmitter();
  const heard: string[] = [];
  const first = (error: Error) => heard.push("first " + error.message);
  const second = (error: Error) => heard.push("second " + error.message);
  two.on("uncaughtException", first);
  two.on("uncaughtException", second);
  quietClosedSocketReads(two, (error) => recorded.push(error));
  assert.deepEqual(two.rawListeners("uncaughtException"), [first, second]);
  two.emit("uncaughtException", reported, "uncaughtException");
  assert.deepEqual(heard, ["first " + reported.message, "second " + reported.message]);
  // One added with once(): it still hears only the first error, and the
  // next finds no listener, as before.
  const once = new EventEmitter();
  const heardOnce: Error[] = [];
  once.once("uncaughtException", (error: Error) => heardOnce.push(error));
  const wrapper = once.rawListeners("uncaughtException")[0];
  quietClosedSocketReads(once, (error) => recorded.push(error));
  assert.deepEqual(once.rawListeners("uncaughtException"), [wrapper]);
  once.emit("uncaughtException", reported, "uncaughtException");
  assert.deepEqual(heardOnce, [reported]);
  assert.equal(once.listenerCount("uncaughtException"), 0);
  assert.deepEqual(recorded, [], "nothing is noted where nothing was installed");
});
