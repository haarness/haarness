import test from "node:test";
import assert from "node:assert/strict";
import { cachedResources, resourceOptions, searchResources, type ResourceOption } from "../src/renderer/resource-mentions";

const setup: ResourceOption = { server: "docs", uri: "docs://setup", name: "setup", description: "Setup guide" };
const tables: ResourceOption = { server: "warehouse", uri: "db://setup-tables", name: "tables" };

/** A listing the test answers by hand, with its own cache and clock. */
function lister() {
  const asked: { connection: string; ok: (list: ResourceOption[]) => void; no: () => void }[] = [];
  const cache = new Map();
  let now = 0;
  const ask = (connection: string) =>
    cachedResources(
      connection,
      () => new Promise<ResourceOption[]>((ok, no) => asked.push({ connection, ok, no: () => no(new Error("down")) })),
      { cache, now },
    );
  return { asked, cache, ask, tick: (ms: number) => (now += ms) };
}

test("the first `@` asks once; another within a minute, from any composer, reuses that answer", async () => {
  const list = lister();
  const first = list.ask("project");
  assert.equal(list.asked.length, 1);
  list.asked[0].ok([setup]);
  assert.deepEqual(await first, [setup]);
  list.tick(59_000);
  // The same answer, not a copy: a copy would make the `@` list redraw its rows.
  assert.equal(list.ask("project"), first);
  assert.equal(list.asked.length, 1);
  list.tick(1_000);
  const again = list.ask("project");
  assert.notEqual(again, first);
  assert.equal(list.asked.length, 2);
});

test("each project asks on its own, and a project not visited for a minute is dropped", async () => {
  const list = lister();
  list.ask("one");
  list.asked[0].ok([setup]);
  list.tick(30_000);
  list.ask("two");
  list.asked[1].ok([tables]);
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
  await assert.rejects(failed);
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

test("rows match by name only, those starting with it first, and stop at fifty", () => {
  const many = Array.from({ length: 80 }, (_, index) => ({ server: "docs", uri: `docs://${index}`, name: `page ${index}` }));
  assert.equal(searchResources("", many).length, 50);
  assert.deepEqual(searchResources("SET", [tables, setup]), [setup]);
  // The URI holds "setup" too, but only names count.
  assert.deepEqual(searchResources("setup", [tables]), []);
  assert.deepEqual(
    searchResources("up", [setup, { ...tables, name: "upload" }]).map((item) => item.name),
    ["upload", "setup"],
  );
});
