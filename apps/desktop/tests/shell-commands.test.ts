import test from "node:test";
import assert from "node:assert/strict";
import type { Item, ItemData, ShellCommandInfo } from "@haarness/sdk";
import {
  commandCheckpoints,
  commandForCall,
  commandForgotten,
  goneKey,
  commandLines,
  moveRefusal,
  stepCallId,
  commandStatus,
  commandView,
  createOutputFollow,
  elapsed,
  laterCommand,
  liveCommand,
  ranFor,
  screenText,
  type FollowView,
  type OutputPage,
} from "../src/renderer/shell-commands";

const info = (patch: Partial<ShellCommandInfo> = {}): ShellCommandInfo => ({
  id: "c1",
  threadId: "chat",
  rootThreadId: "chat",
  turnId: "turn",
  callId: "call",
  command: "npm run dev",
  background: true,
  movedBy: "agent",
  state: "running",
  startedAt: "2026-10-29T10:00:00Z",
  outputBytes: 0,
  deliversNotice: true,
  ...patch,
});
const item = (sequence: number, data: ItemData, turnId = "turn"): Item => ({
  id: `item-${sequence}`,
  threadId: "chat",
  turnId,
  sequence,
  createdAt: "2026-10-29T10:00:00Z",
  data,
});
const checkpoint = (state: string, patch: Record<string, unknown> = {}) =>
  ({
    type: "command_state",
    call_id: "call",
    command_id: "c1",
    state,
    started_at: "2026-10-29T10:00:00Z",
    tail: "",
    ...patch,
  }) as Extract<ItemData, { type: "command_state" }>;

test("a command's state reads in the shared words, and only running and stopping are live", () => {
  const word = (patch: Parameters<typeof commandStatus>[0]) => commandStatus(patch);
  assert.deepEqual(word({ state: "running" }), { key: "Running", tone: "live" });
  assert.deepEqual(word({ state: "stopping" }), { key: "Stopping…", tone: "live" });
  assert.deepEqual(word({ state: "exited", exitCode: 0 }), { key: "Completed", tone: "done" });
  assert.deepEqual(word({ state: "exited", exitCode: 2 }), {
    key: "exit {code}",
    values: { code: "2" },
    tone: "failed",
  });
  // Killed by a signal nobody in Haarness sent: no code to give.
  assert.deepEqual(word({ state: "exited", signal: "SIGSEGV" }), { key: "Failed", tone: "failed" });
  assert.equal(word({ state: "stopped", exitCode: 130 })?.key, "Stopped");
  assert.equal(word({ state: "timed_out" })?.key, "Ran out of time");
  assert.equal(word({ state: "interrupted", endedAt: "2026-10-29T10:05:00Z" })?.key, "Cut off by a restart");
  // A fork's copy of a command that runs on elsewhere was never cut off.
  assert.equal(word({ state: "interrupted" }), undefined);
  // A state a newer server reports says nothing rather than something wrong.
  assert.equal(word({ state: "unknown" }), undefined);
  assert.equal(word({}), undefined);
  assert.equal(liveCommand("running"), true);
  assert.equal(liveCommand("stopping"), true);
  for (const state of ["exited", "stopped", "timed_out", "interrupted", "unknown", undefined])
    assert.equal(liveCommand(state), false, String(state));
});

test("how long a command ran reads narrow, in hours past an hour", () => {
  assert.equal(ranFor(0, "en"), "1s");
  assert.equal(ranFor(12_400, "en"), "12s");
  assert.equal(ranFor(72_000, "en"), "1m 12s");
  assert.equal(ranFor(59 * 60_000 + 59_000, "en"), "59m 59s");
  assert.equal(ranFor(60 * 60_000, "en"), "1h");
  assert.equal(ranFor(125 * 60_000 + 30_000, "en"), "2h 5m");
  assert.equal(elapsed("2026-10-29T10:00:00Z", "2026-10-29T10:01:12Z"), 72_000);
  assert.equal(elapsed("2026-10-29T10:00:00Z", Date.parse("2026-10-29T09:59:00Z")), 0);
  assert.equal(elapsed(undefined, Date.now()), undefined);
});

test("a row knows its command from the server first, then the chat's checkpoint, then its result", () => {
  // A background command the server still reports.
  const live = commandView({
    result: { metadata: { command: { id: "c1", background: true, startedAt: "2026-10-29T10:00:00Z" } } },
    info: info({ state: "stopping" }),
    checkpoint: checkpoint("running"),
  });
  assert.equal(live?.state, "stopping");
  assert.equal(live?.background, true);
  // Gone from the server's list: the last checkpoint says how it ended and
  // keeps the end of its output.
  const ended = commandView({
    result: { metadata: { command: { id: "c1", background: true } } },
    checkpoint: checkpoint("exited", { exit_code: 2, ended_at: "2026-10-29T10:05:00Z", tail: "failed\n" }),
  });
  assert.deepEqual(
    [ended?.state, ended?.exitCode, ended?.endedAt, ended?.tail, ended?.background],
    ["exited", 2, "2026-10-29T10:05:00Z", "failed\n", true],
  );
  // A foreground command that ended in the foreground: only its result
  // names it, and it is no background row.
  const foreground = commandView({ result: { metadata: { exit: 0, command: { id: "c2", background: false } } } });
  assert.deepEqual([foreground?.id, foreground?.background, foreground?.state], ["c2", false, undefined]);
  // A foreground command the server reports while it runs, before any result.
  const running = commandView({ info: info({ background: false, movedBy: undefined }) });
  assert.deepEqual([running?.background, running?.state], [false, "running"]);
  // Once the server has listed the chat's commands without it, a checkpoint
  // that says it runs is from before (a fork copies them): not live.
  const copied = commandView({ checkpoint: checkpoint("running"), forgotten: true });
  assert.deepEqual([copied?.background, copied?.state], [true, undefined]);
  const kept = commandView({ checkpoint: checkpoint("exited", { exit_code: 0 }), forgotten: true });
  assert.equal(kept?.state, "exited");
  // What the server reports always wins.
  assert.equal(commandView({ info: info(), checkpoint: checkpoint("running"), forgotten: true })?.state, "running");
  // Nothing about a command: an older server's row stays as it was.
  assert.equal(commandView({ result: { metadata: { exit: 0 } } }), undefined);
  assert.equal(commandView({}), undefined);
});

test("checkpoints are read per call, the latest one winning, and a row finds its command", () => {
  const items = [
    item(1, { type: "tool_call", call_id: "call", name: "bash", arguments: { command: "npm run dev" } }),
    item(2, checkpoint("running")),
    item(3, checkpoint("stopped", { ended_at: "2026-10-29T10:05:00Z" })),
    // The same call id in a later turn is another command.
    item(4, checkpoint("running", { command_id: "c9" }), "later"),
  ];
  const found = commandCheckpoints(items);
  assert.equal(found.get("turn:call")?.state, "stopped");
  assert.equal(found.get("later:call")?.command_id, "c9");
  const commands = [info(), info({ id: "c2", callId: "other" })];
  assert.equal(commandForCall(commands, { callId: "other", turnId: "turn" })?.id, "c2");
  assert.equal(commandForCall(commands, { id: "c1", callId: "other", turnId: "turn" })?.id, "c1");
  assert.equal(commandForCall(commands, { callId: "call", turnId: "another" }), undefined);
});

test("a fork's copy never speaks for the command of the chat it came from", () => {
  const gone = new Set<string>();
  const forgotten = (thread: string, patch: Partial<Parameters<typeof commandForgotten>[0]> = {}) =>
    commandForgotten({ id: "c1", thread, reported: false, gone: (key) => gone.has(key), listed: false, ...patch });
  // The server reports c1 as the original's: the fork's row is not live,
  // even before the fork's own list has loaded.
  assert.equal(forgotten("fork", { owner: "original" }), true);
  // The fork's read of it answered not found: remembered for the fork alone.
  gone.add(goneKey("fork", "c1"));
  assert.equal(forgotten("fork"), true);
  assert.equal(forgotten("original"), false);
  assert.equal(forgotten("original", { reported: true }), false);
  // Listed for the chat without it, or answered gone for that chat.
  assert.equal(forgotten("other", { listed: true }), true);
  // Not known yet anywhere and nothing listed: its checkpoint stands.
  assert.equal(forgotten("other"), false);
  assert.equal(commandForgotten({ thread: "fork", reported: false, gone: () => true, listed: true }), false);
});

test("a batch's steps are numbered after it, and each command's line is known by its id", () => {
  assert.equal(stepCallId("call_7", 0), "call_7:1");
  assert.equal(stepCallId("call_7", 2), "call_7:3");
  const items = [
    item(1, { type: "tool_call", call_id: "a", name: "bash", arguments: { command: "npm run dev", run_in_background: true } }),
    item(2, { type: "tool_result", call_id: "a", name: "bash", is_error: false, output: "note", metadata: { command: { id: "c1", background: true } } }),
    item(3, {
      type: "tool_call",
      call_id: "b",
      name: "batch",
      arguments: { tool_calls: [{ tool: "read", parameters: { path: "a" } }, { tool: "bash", parameters: { command: "cargo watch" } }] },
    }),
    item(4, {
      type: "tool_result",
      call_id: "b",
      name: "batch",
      is_error: false,
      output: "{}",
      metadata: { results: [{ tool: "read", output: "x" }, { tool: "bash", output: "note", metadata: { command: { id: "c2", background: true } } }] },
    }),
    // A call id reused by a later turn is another call.
    item(5, { type: "tool_call", call_id: "a", name: "bash", arguments: { command: "ls" } }, "later"),
    item(6, { type: "tool_result", call_id: "a", name: "bash", is_error: false, output: "x", metadata: { command: { id: "c3", background: false } } }, "later"),
  ];
  const lines = commandLines(items);
  assert.equal(lines.get("c1"), "npm run dev");
  assert.equal(lines.get("c2"), "cargo watch");
  assert.equal(lines.get("c3"), "ls");
  assert.equal(lines.size, 3);
});

test("a late report never brings an ended command back or takes it out of the background", () => {
  const stopping = info({ state: "stopping" });
  assert.equal(laterCommand(stopping, info()), stopping);
  const stopped = info({ state: "stopped", endedBy: "person" });
  assert.equal(laterCommand(stopped, info({ state: "stopping" })), stopped);
  const moved = info({ background: true, movedBy: "person" });
  assert.equal(laterCommand(moved, info({ background: false })), moved);
  // Newer news replaces older at the same stage, and a first report is kept.
  const grown = info({ outputBytes: 512 });
  assert.equal(laterCommand(info(), grown), grown);
  assert.equal(laterCommand(undefined, grown), grown);
  assert.equal(laterCommand(info({ state: "running" }), stopped), stopped);
});

test("output reads as the terminal left it, without escapes and with rewritten lines settled", () => {
  assert.equal(screenText("\u001b[32mok\u001b[0m\n"), "ok\n");
  assert.equal(screenText("Building 10%\rBuilding 50%\rBuilding 100%\ndone\n"), "Building 100%\ndone\n");
  assert.equal(screenText("line\r\nnext\r\n"), "line\nnext\n");
  // A carriage return at the end has not rewritten anything yet.
  assert.equal(screenText("step 1\r"), "step 1");
});

/** A server's output for a command that prints `chunks` over time. */
function fakeServer(options: { size?: number } = {}) {
  let output = "";
  let state: OutputPage["state"] = "running";
  let exitCode: number | undefined;
  const reads: { cursor?: number; limit?: number; tail?: boolean }[] = [];
  let failNext: Error | undefined;
  let gate: Promise<void> | undefined;
  const limit = options.size ?? 1 << 20;
  return {
    reads,
    print(text: string) {
      output += text;
    },
    end(code: number) {
      state = "exited";
      exitCode = code;
    },
    fail(error: Error) {
      failNext = error;
    },
    hold() {
      let release!: () => void;
      gate = new Promise((resolve) => (release = resolve));
      return () => {
        gate = undefined;
        release();
      };
    },
    async read(query: { cursor?: number; limit?: number; tail?: boolean }): Promise<OutputPage> {
      reads.push(query);
      if (gate) await gate;
      if (failNext) {
        const error = failNext;
        failNext = undefined;
        throw error;
      }
      const bytes = Math.min(query.limit ?? limit, limit);
      const start = query.tail ? Math.max(0, output.length - bytes) : Math.min(query.cursor ?? 0, output.length);
      const end = Math.min(output.length, start + bytes);
      return {
        text: output.slice(start, end),
        start,
        cursor: end,
        size: output.length,
        more: end < output.length,
        settled: state !== "running" && end >= output.length,
        state,
        exitCode,
      };
    },
  };
}
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
const noTimer = { set: () => undefined, clear: () => {} };

test("a followed command reads from where it last read, on each wake, until it has settled", async () => {
  const server = fakeServer();
  const views: FollowView[] = [];
  const follow = createOutputFollow({ read: server.read, update: (view) => views.push(view), timer: noTimer });
  server.print("starting\n");
  follow.wake();
  await settle();
  assert.deepEqual(server.reads[0], { tail: true, limit: 64 * 1024 });
  assert.equal(views.at(-1)?.text, "starting\n");
  assert.equal(views.at(-1)?.read, true);
  server.print("ready on :3000\n");
  follow.wake();
  await settle();
  assert.deepEqual(server.reads[1], { cursor: 9, limit: 64 * 1024 });
  assert.equal(views.at(-1)?.text, "starting\nready on :3000\n");
  server.print("bye\n");
  server.end(0);
  follow.wake();
  await settle();
  assert.equal(views.at(-1)?.settled, true);
  assert.equal(views.at(-1)?.state, "exited");
  assert.equal(views.at(-1)?.exitCode, 0);
  // Settled: nothing more is read, whatever wakes it.
  const count = server.reads.length;
  follow.wake();
  await settle();
  assert.equal(server.reads.length, count);
  follow.dispose();
});

test("a wake during a read reads once more after it, never two reads at once", async () => {
  const server = fakeServer();
  const views: FollowView[] = [];
  const follow = createOutputFollow({ read: server.read, update: (view) => views.push(view), timer: noTimer });
  server.print("a\n");
  const release = server.hold();
  follow.wake();
  follow.wake();
  follow.wake();
  await settle();
  assert.equal(server.reads.length, 1);
  server.print("b\n");
  release();
  await settle();
  await settle();
  assert.equal(server.reads.length, 2);
  assert.equal(views.at(-1)?.text, "a\nb\n");
  follow.dispose();
});

test("a long output keeps its end from the start of a line, and a gap is marked", async () => {
  const server = fakeServer({ size: 16 });
  const views: FollowView[] = [];
  const follow = createOutputFollow({ read: server.read, update: (view) => views.push(view), keep: 16, timer: noTimer });
  server.print("first line here\nsecond\nthird\n");
  follow.wake();
  await settle();
  // The tail began inside "first line here": that partial line is left out.
  assert.equal(views.at(-1)?.text, "second\nthird\n");
  assert.equal(views.at(-1)?.cut, true);
  // More than the server reads at once arrives: it is read page by page,
  // and only the end is kept.
  server.print("0123456789\nabcdefghij\nend\n");
  follow.wake();
  await settle();
  await settle();
  assert.equal(views.at(-1)?.text, "abcdefghij\nend\n");
  follow.dispose();
});

test("output the server dropped between two reads is marked where it was", async () => {
  let page = 0;
  const pages: OutputPage[] = [
    { text: "one\n", start: 0, cursor: 4, size: 4, more: false, settled: false, state: "running" },
    { text: "ten\n", start: 40, cursor: 44, size: 44, more: false, settled: false, state: "running" },
  ];
  const views: FollowView[] = [];
  const follow = createOutputFollow({ read: async () => pages[page++], update: (view) => views.push(view), timer: noTimer });
  follow.wake();
  await settle();
  follow.wake();
  await settle();
  assert.equal(views.at(-1)?.text, "one\n…\nten\n");
  assert.equal(views.at(-1)?.cut, true);
  follow.dispose();
});

test("a command the server no longer has stops the follow; other failures try again", async () => {
  const server = fakeServer();
  const views: FollowView[] = [];
  const timers: (() => void)[] = [];
  const follow = createOutputFollow({
    read: server.read,
    update: (view) => views.push(view),
    timer: { set: (run) => timers.push(run), clear: () => {} },
    // As Desktop asks: does the chat's list still hold it?
    gone: async (error) => /not found/.test(String(error)),
  });
  server.fail(new Error("Gateway hiccup"));
  follow.wake();
  await settle();
  assert.equal(views.length, 0);
  // The poll tries again.
  assert.equal(timers.length, 1);
  server.print("back\n");
  timers.shift()!();
  await settle();
  assert.equal(views.at(-1)?.text, "back\n");
  server.fail(new Error("404 not_found: command not found"));
  follow.wake();
  await settle();
  assert.equal(views.at(-1)?.gone, true);
  const count = server.reads.length;
  follow.wake();
  await settle();
  assert.equal(server.reads.length, count);
  follow.dispose();
});

test("a computer offline or a workspace asleep never wipes what was read, however long it lasts", async () => {
  const server = fakeServer();
  const views: FollowView[] = [];
  const timers: (() => void)[] = [];
  let asked = 0;
  const follow = createOutputFollow({
    read: server.read,
    update: (view) => views.push(view),
    timer: { set: (run) => timers.push(run), clear: () => {} },
    // The list cannot be read either: it is passing.
    gone: async () => {
      asked++;
      return false;
    },
  });
  server.print("serving\n");
  follow.wake();
  await settle();
  for (let attempt = 0; attempt < 12; attempt++) {
    server.fail(new Error(attempt % 2 ? "409 conflict: the computer is offline" : "400: the workspace is asleep"));
    timers.shift()!();
    await settle();
  }
  assert.equal(asked, 12);
  assert.equal(views.length, 1);
  assert.equal(views[0].gone, false);
  assert.equal(views[0].text, "serving\n");
  // It comes back and reads on from where it was.
  server.print("still here\n");
  timers.shift()!();
  await settle();
  assert.equal(views.at(-1)?.text, "serving\nstill here\n");
  follow.dispose();
});

test("failed reads space the next ones out, and a hidden window reads nothing until it shows", async () => {
  const server = fakeServer();
  const views: FollowView[] = [];
  const waits: number[] = [];
  const timers: (() => void)[] = [];
  let hidden = false;
  const follow = createOutputFollow({
    read: server.read,
    update: (view) => views.push(view),
    timer: {
      set: (run, ms) => {
        waits.push(ms);
        timers.push(run);
      },
      clear: () => {},
    },
    poll: 1000,
    maxWait: 6000,
    paused: () => hidden,
  });
  server.print("a\n");
  follow.wake();
  await settle();
  for (let attempt = 0; attempt < 4; attempt++) {
    server.fail(new Error("409 conflict: the computer is offline"));
    timers.shift()!();
    await settle();
  }
  // 1 s after a read, then 2, 4 and at most 6 s after each failure.
  assert.deepEqual(waits, [1000, 2000, 4000, 6000, 6000]);
  timers.shift()!();
  await settle();
  assert.equal(waits.at(-1), 1000);
  // Hidden: a wake or a poll reads nothing, and showing again catches up.
  const reads = server.reads.length;
  hidden = true;
  server.print("b\n");
  follow.wake();
  timers.shift()?.();
  await settle();
  assert.equal(server.reads.length, reads);
  hidden = false;
  follow.resume();
  await settle();
  assert.equal(server.reads.length, reads + 1);
  assert.equal(views.at(-1)?.text, "a\nb\n");
  follow.dispose();
});

test("a refused move reads as the server's plain reason, or not at all", () => {
  assert.equal(moveRefusal("conflict: this chat runs as many commands in the background as it may"), "This chat runs as many commands in the background as it may.");
  assert.equal(moveRefusal("Conflict: The command has already ended."), "The command has already ended.");
  // Ids, long or structured reasons are left for Desktop to say itself.
  assert.equal(moveRefusal("conflict: Command cmd-4f2a91 cannot move to the background: it has ended"), undefined);
  assert.equal(moveRefusal("conflict: thread 01a18877-f7b5-7e40-9037-54d6778d66b3 is busy"), undefined);
  assert.equal(moveRefusal("conflict: " + "it cannot move ".repeat(12)), undefined);
  assert.equal(moveRefusal("conflict: {\"reason\":\"cap\"}"), undefined);
  assert.equal(moveRefusal("conflict: "), undefined);
});

test("a follow that waited out failures reads at once when the connection is back", async () => {
  const server = fakeServer();
  const waits: number[] = [];
  const timers: (() => void)[] = [];
  const follow = createOutputFollow({
    read: server.read,
    update: () => {},
    timer: {
      set: (run, ms) => {
        waits.push(ms);
        timers.push(run);
      },
      clear: () => {},
    },
    poll: 1000,
  });
  follow.wake();
  await settle();
  for (let attempt = 0; attempt < 3; attempt++) {
    server.fail(new Error("409 conflict: the computer is offline"));
    timers.shift()!();
    await settle();
  }
  assert.equal(waits.at(-1), 8000);
  const reads = server.reads.length;
  follow.nudge();
  await settle();
  assert.equal(server.reads.length, reads + 1);
  assert.equal(waits.at(-1), 1000);
  follow.dispose();
});

test("a disposed follow drops a read still in flight and polls no more", async () => {
  const server = fakeServer();
  const views: FollowView[] = [];
  const cleared: unknown[] = [];
  const follow = createOutputFollow({
    read: server.read,
    update: (view) => views.push(view),
    timer: { set: () => "timer", clear: (handle) => cleared.push(handle) },
  });
  server.print("x\n");
  const release = server.hold();
  follow.wake();
  follow.dispose();
  release();
  await settle();
  assert.equal(views.length, 0);
  assert.ok(cleared.length > 0);
});
