import assert from "node:assert/strict";
import test from "node:test";
import { tabID } from "../src/core/browser";
import { createRegistry, threadKey, wireState, wireTab, type Snapshot, type TabRecord } from "../src/core/registry";

function memory() {
  let saved: Snapshot | undefined;
  return { load: async () => saved && structuredClone(saved), save: async (snapshot: Snapshot) => void (saved = structuredClone(snapshot)) };
}
const record = (n: number): TabRecord => ({
  id: tabID(`00000000-0000-0000-0000-${String(n).padStart(12, "0")}`),
  tabId: n,
  generation: 0,
  url: "https://a.example/",
  title: "",
  loading: false,
  canGoBack: false,
  canGoForward: false,
});

test("chats are keyed by project and thread, and survive a worker restart", async () => {
  const storage = memory();
  const registry = createRegistry(storage);
  const entry = registry.ensure("/work/a", "t1");
  registry.addTab(entry, record(1));
  assert.equal(registry.owner(1)?.entry, entry);
  assert.equal(registry.get(threadKey("/work/a", "t1")), entry);
  assert.notEqual(registry.ensure("/work/b", "t1"), entry, "the same thread id in another project is another chat");
  await registry.flush();
  const restarted = createRegistry(storage);
  await restarted.load();
  assert.equal(restarted.owner(1)?.entry.thread, "t1");
});

test("a new server instance for a project clears that project's approvals only", () => {
  const registry = createRegistry(memory());
  const a = registry.ensure("/work/a", "t1");
  const b = registry.ensure("/work/b", "t2");
  registry.approve(a, ["https://a.example"]);
  registry.approve(b, ["https://b.example"]);
  assert.equal(registry.attach("/work/a", "i1"), false, "first sight is not a restart");
  assert.equal(registry.attach("/work/a", "i1"), false);
  assert.deepEqual(a.approved, ["https://a.example"]);
  assert.equal(registry.attach("/work/a", "i2"), true);
  assert.deepEqual(a.approved, []);
  assert.deepEqual(b.approved, ["https://b.example"]);
});

test("revoked tabs are remembered (newest 32) and the focused tab moves on", () => {
  const registry = createRegistry(memory());
  const entry = registry.ensure("/work/a", "t1");
  for (let n = 1; n <= 40; n++) registry.addTab(entry, record(n));
  assert.equal(entry.focusedTabID, record(1).id);
  registry.removeTab(entry, record(1).id, true);
  assert.equal(entry.focusedTabID, record(2).id);
  for (let n = 2; n <= 40; n++) registry.removeTab(entry, record(n).id, n % 2 === 0);
  assert.ok(entry.revoked.length <= 32);
  assert.equal(entry.revoked.at(-1), record(40).id);
  assert.equal(registry.owner(5), undefined);
});

test("published tabs stay inside what the server accepts", () => {
  const long = { ...record(1), url: "https://a.example/" + "x".repeat(20_000), title: "t".repeat(3_000), generation: 2.7 };
  const tab = wireTab(long);
  assert.equal(tab.url.length, 16_384);
  assert.equal(tab.title.length, 2_048);
  assert.equal(tab.generation, 2);
  const registry = createRegistry(memory());
  const entry = registry.ensure("/w", "t");
  assert.deepEqual(wireState(entry), { tabs: [], focusedTabID: null });
});

test("the oldest idle chats are forgotten past 256", () => {
  let now = 0;
  const registry = createRegistry(memory(), () => now++);
  const busy = registry.ensure("/w", "busy");
  registry.addTab(busy, record(1));
  for (let n = 0; n < 300; n++) registry.ensure("/w", `idle-${n}`);
  assert.equal(registry.all().length, 256);
  assert.ok(registry.get(threadKey("/w", "busy")));
  assert.equal(registry.get(threadKey("/w", "idle-0")), undefined);
});
