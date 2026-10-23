import test from "node:test";
import assert from "node:assert/strict";
import {
  resourceEntry,
  resourceOptions,
  searchResources,
  type Entry,
  type ResourceOption,
} from "../src/renderer/resource-mentions";

const setup: ResourceOption = { server: "docs", uri: "docs://setup", name: "setup", description: "Setup guide" };
const tables: ResourceOption = { server: "warehouse", uri: "db://setup-tables", name: "tables" };
const wiki: ResourceOption = { ...setup, server: "wiki" };

/** A listing the test answers by hand, server by server, with its own cache and clock. */
function lister() {
  const asked: { connection: string; add: (rows: ResourceOption[]) => void; ok: () => void; no: () => void }[] = [];
  const cache = new Map<string, Entry>();
  let now = 0;
  const ask = (connection: string) =>
    resourceEntry(
      connection,
      (add) => new Promise<void>((ok, no) => asked.push({ connection, add, ok, no: () => no(new Error("down")) })),
      { cache, now },
    );
  return { asked, cache, ask, tick: (ms: number) => (now += ms) };
}

test("the first `@` asks once; another within a minute, from any composer, reuses that listing", () => {
  const list = lister();
  const first = list.ask("project");
  assert.equal(list.asked.length, 1);
  list.asked[0].add([setup]);
  assert.deepEqual(first.rows(), [setup]);
  list.tick(59_000);
  // The same listing, not a copy: a copy would make the `@` list redraw its rows.
  assert.equal(list.ask("project"), first);
  assert.equal(list.asked.length, 1);
  list.tick(1_000);
  const again = list.ask("project");
  assert.notEqual(again, first);
  assert.equal(list.asked.length, 2);
  // The earlier rows stay until the new listing's first answer.
  assert.deepEqual(again.rows(), [setup]);
  list.asked[1].add([tables]);
  assert.deepEqual(again.rows(), [tables]);
});

test("each server's rows join below the others as they come, so a slow one holds nothing up", async () => {
  const list = lister();
  const entry = list.ask("project");
  list.asked[0].add([wiki]);
  assert.deepEqual(entry.rows(), [wiki]);
  list.asked[0].add([setup, tables]);
  assert.deepEqual(entry.rows(), [wiki, setup, tables]);
  list.asked[0].ok();
  await entry.done;
  assert.deepEqual(entry.rows(), [wiki, setup, tables]);
});

test("each project asks on its own, and a project not visited for a minute is dropped", () => {
  const list = lister();
  list.ask("one");
  list.tick(30_000);
  list.ask("two");
  assert.deepEqual([...list.cache.keys()], ["one", "two"]);
  list.tick(31_000);
  // `one` is now a minute old: asking for `two` again drops it.
  list.ask("two");
  assert.deepEqual([...list.cache.keys()], ["two"]);
});

test("a failed listing is forgotten, so the next `@` asks again", async () => {
  const list = lister();
  const failed = list.ask("project");
  list.asked[0].no();
  await failed.done;
  assert.equal(list.cache.size, 0);
  list.ask("project");
  assert.equal(list.asked.length, 2);
});

test("a server's listing becomes rows: a URI is required, the name falls back to it", () => {
  assert.deepEqual(
    resourceOptions("docs", [
      { uri: "docs://setup", name: "setup", title: "Setup guide", mimeType: "text/markdown" },
      { uri: "docs://logo" },
      { name: "no address" },
      null,
      "text",
    ]),
    [
      { server: "docs", uri: "docs://setup", name: "setup", description: "Setup guide", mimeType: "text/markdown" },
      { server: "docs", uri: "docs://logo", name: "docs://logo", description: undefined, mimeType: undefined },
    ],
  );
});

test("rows match by name only, server by server, those starting with it first, and stop at fifty", () => {
  const many = Array.from({ length: 80 }, (_, index) => ({ server: "docs", uri: `docs://${index}`, name: `page ${index}` }));
  assert.equal(searchResources("", many).length, 50);
  assert.deepEqual(searchResources("SET", [tables, setup]), [setup]);
  // The URI holds "setup" too, but only names count.
  assert.deepEqual(searchResources("setup", [tables]), []);
  const upload = { ...tables, name: "upload" };
  // Within a server, names that start with it first; servers keep the order they answered in.
  assert.deepEqual(
    searchResources("up", [setup, { ...upload, server: "docs" }, upload]).map((item) => `${item.server}:${item.name}`),
    ["docs:upload", "docs:setup", "warehouse:upload"],
  );
  // A server the chat's rules take away is left out.
  assert.deepEqual(searchResources("set", [setup, wiki], new Set(["docs"])), [wiki]);
});
