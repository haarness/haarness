import test from "node:test";
import assert from "node:assert/strict";
import { createMarkdownClient } from "../src/renderer/markdown-client";
import { projectMarkdown, markdownPatch, type MarkdownProjection, type MarkdownPatch } from "../src/renderer/markdown-project";

test("worker transport coalesces updates, suppresses stale replies, disposes owners and falls back", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const original = globalThis.Worker;
  type Request = { key: number; revision: number; text?: string; live?: boolean };
  const workers: Transport[] = [];
  class Transport {
    messages: Request[] = [];
    states = new Map<number, MarkdownProjection>();
    onmessage?: (event: unknown) => void;
    onerror?: (event: unknown) => void;
    onmessageerror?: () => void;
    terminated = false;
    constructor() { workers.push(this); }
    postMessage(request: Request) { this.messages.push(request); }
    terminate() { this.terminated = true; }
    deliver() {
      const request = this.messages.shift()!;
      if (request.text === undefined) { this.states.delete(request.key); return; }
      const previous = this.states.get(request.key);
      const next = projectMarkdown(previous, request.text, !!request.live);
      this.states.set(request.key, next);
      this.onmessage?.({ data: { key: request.key, revision: request.revision, patch: markdownPatch(previous, next) } });
    }
  }
  globalThis.Worker = Transport as unknown as typeof Worker;
  const delivered: string[] = [];
  let html: string[] = [];
  const receive = (patch: MarkdownPatch) => {
    html.splice(patch.start, Infinity, ...patch.html);
    html.length = patch.count;
    delivered.push(html.join(""));
  };
  const client = createMarkdownClient(receive);
  try {
    client.update("Old conversation", true);
    client.update("An intermediate update", true);
    client.update("# Current conversation", false);
    assert.equal(workers.length, 1);
    assert.equal(workers[0].messages.length, 1);
    workers[0].deliver();
    assert.deepEqual(delivered, []);
    assert.equal(workers[0].messages.length, 1);
    assert.equal(workers[0].messages[0].text, "# Current conversation");
    workers[0].deliver();
    assert.deepEqual(delivered, ["<h1>Current conversation</h1>\n"]);
    client.update("# Current conversation\n\nA later paragraph", true);
    workers[0].onmessageerror?.();
    assert.equal(workers[0].terminated, true);
    assert.equal(delivered.at(-1), "<h1>Current conversation</h1>\n<p>A later paragraph</p>\n");
    client.update("Replacement after failure", false);
    assert.equal(delivered.at(-1), "<p>Replacement after failure</p>\n");
    const count = delivered.length;
    client.dispose();
    workers[0].deliver();
    client.update("Must not render after disposal", true);
    assert.equal(delivered.length, count);
    context.mock.timers.tick(30_000);
    const second = createMarkdownClient(receive);
    second.update("Fresh worker", false);
    assert.equal(workers.length, 2);
    second.dispose();
    workers[1].deliver();
    assert.equal(delivered.length, count);
    context.mock.timers.tick(30_000);
    assert.equal(workers[1].terminated, true);
  } finally {
    client.dispose();
    context.mock.timers.tick(30_000);
    globalThis.Worker = original;
  }
});
