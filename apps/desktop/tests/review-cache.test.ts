import { test } from "node:test";
import assert from "node:assert/strict";
import { ReviewCache } from "../src/renderer/review-cache";
import type { FileDiff } from "@haarness/sdk";

const file = (path: string): FileDiff => ({
  file: path,
  status: "modified",
  additions: 1,
  deletions: 1,
  patch: "replacement",
});
test("review reads are deduplicated, concurrency bounded and disposed queues do not read", async () => {
  const releases: (() => void)[] = [];
  const calls: string[] = [];
  const cache = new ReviewCache(async (path) => {
    calls.push(path);
    await new Promise<void>((resolve) => releases.push(resolve));
    return file(path);
  });
  const first = cache.load("first");
  assert.equal(cache.load("first"), first);
  const pending = [
    first,
    ...Array.from({ length: 20 }, (_, i) => cache.load(String(i))),
  ];
  assert.equal(calls.length, 3);
  cache.dispose();
  releases.forEach((resolve) => resolve());
  assert.deepEqual(await Promise.all(pending), Array(21).fill(undefined));
  assert.equal(calls.length, 3);
});

test("review cache retries failed reads and evicts least recent bodies", async () => {
  const calls: string[] = [];
  const cache = new ReviewCache(
    async (path) => {
      calls.push(path);
      return file(path);
    },
    1000,
    2,
  );
  await cache.load("a");
  await cache.load("b");
  await cache.load("a");
  await cache.load("c");
  await cache.load("b");
  assert.deepEqual(calls, ["a", "b", "c", "b"]);
  let attempts = 0;
  const retry = new ReviewCache(async (path) => {
    if (++attempts === 1) throw Error("temporary");
    return file(path);
  });
  await assert.rejects(retry.load("a"), /temporary/);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal((await retry.load("a"))?.file, "a");
});

test("review cache bounds large bodies by bytes as well as entry count", async () => {
  let calls = 0;
  const cache = new ReviewCache(async (path) => {
    calls++;
    return file(path);
  }, 1);
  await cache.load("large");
  await new Promise((resolve) => setImmediate(resolve));
  await cache.load("large");
  assert.equal(calls, 2);
});
