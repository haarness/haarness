import test from "node:test";
import assert from "node:assert/strict";
import {
  connectionSummary,
  fallbackCatalog,
  filterProviders,
  groupProviders,
  keyLabel,
  keyWarning,
  parseCatalog,
  parseLogin,
  usableMethods,
} from "../src/renderer/connect-provider";

const view = {
  title: "Connect a provider",
  data: {
    providers: [
      {
        id: "openai",
        name: "OpenAI",
        connected: true,
        models: 12,
        source: "oauth",
        popular: 0,
        methods: [
          { id: "browser", kind: "oauth", label: "Sign in with ChatGPT", description: "Plus or Pro" },
          { id: "device", kind: "oauth", label: "Sign in with a code", description: "Remote" },
          { id: "api", kind: "api", label: "Use an API key", description: "Pay as you go" },
        ],
        keyUrl: "https://platform.openai.com/api-keys",
        env: ["OPENAI_API_KEY"],
      },
      { id: "zai", name: "Z.AI", connected: false, models: 3, methods: [{ id: "api", kind: "api", label: "Use an API key" }] },
      { id: "anthropic", name: "Anthropic", connected: false, models: 4, popular: 1, methods: [{ id: "api", kind: "api", label: "Use an API key" }], keyUrl: "javascript:alert(1)" },
      { id: "cerebras", name: "Cerebras", connected: false, models: 0, methods: [] },
      { name: "no id" },
      "junk",
    ],
  },
};

test("the connect catalog is parsed defensively and keeps only safe key links", () => {
  const catalog = parseCatalog(view as never);
  assert.deepEqual(
    catalog.map((provider) => provider.id),
    ["openai", "zai", "anthropic", "cerebras"],
  );
  assert.equal(catalog[0].keyUrl, "https://platform.openai.com/api-keys");
  assert.equal(catalog[2].keyUrl, null, "non-http links are dropped");
  assert.equal(catalog[0].methods.length, 3);
  assert.deepEqual(catalog[0].env, ["OPENAI_API_KEY"]);
  assert.deepEqual(parseCatalog(undefined), []);
  assert.deepEqual(parseCatalog({ title: "x", data: { providers: "nope" } } as never), []);
});

test("popular providers keep the server's order and the rest is alphabetical", () => {
  const { popular, other } = groupProviders(parseCatalog(view as never));
  assert.deepEqual(popular.map((provider) => provider.id), ["openai", "anthropic"]);
  assert.deepEqual(other.map((provider) => provider.id), ["cerebras", "zai"]);
});

test("search matches ids and names without reordering", () => {
  const catalog = parseCatalog(view as never);
  assert.deepEqual(filterProviders(catalog, "  AI ").map((p) => p.id), ["openai", "zai"]);
  assert.equal(filterProviders(catalog, "").length, catalog.length);
});

test("a remote server hides the browser sign-in but keeps the code", () => {
  const [openai] = parseCatalog(view as never);
  assert.deepEqual(usableMethods(openai, true).map((m) => m.id), ["browser", "device", "api"]);
  assert.deepEqual(usableMethods(openai, false).map((m) => m.id), ["device", "api"]);
});

test("key labels and connection summaries come from the catalog", () => {
  const catalog = parseCatalog(view as never);
  assert.equal(keyLabel(catalog[0]), "API key");
  assert.equal(keyLabel({ ...catalog[0], methods: [{ id: "api", kind: "api", label: "Use a GitHub token", description: "" }] }), "GitHub token");
  assert.deepEqual(connectionSummary(catalog[0]), { how: "signed in", models: 12 });
  assert.deepEqual(connectionSummary({ ...catalog[1], source: "environment" }), { how: "from the environment", models: 3 });
});

test("an old server still gets a key-only picker", () => {
  const catalog = fallbackCatalog([
    { id: "openai", name: "OpenAI", authenticated: false, models: [] },
  ]);
  assert.equal(catalog[0].methods[0].kind, "api");
  assert.equal(catalog[0].popular, null);
});

test("login views are read by state, never by prose", () => {
  assert.deepEqual(
    parseLogin({ title: "Sign in to OpenAI", data: { id: "l1", url: "https://x", code: "AB-CD", method: "device", state: "pending" } } as never),
    { id: "l1", url: "https://x", code: "AB-CD", method: "device", state: "pending", error: null },
  );
  assert.equal(parseLogin({ title: "x", data: { id: "l1", state: "weird" } } as never)?.state, "pending");
  assert.equal(parseLogin({ title: "x", data: { id: "l1", state: "failed", error: "nope" } } as never)?.error, "nope");
  assert.equal(parseLogin({ title: "x" } as never), null);
});

test("a key saved unchecked keeps the server's reason apart from its advice", () => {
  const saved = (warning: string) => ({
    title: "Providers",
    lines: ["Saved. Runtime configuration changes apply when active work is idle.", warning],
    data: { providers: [], warning },
  });
  assert.deepEqual(
    keyWarning(saved("Saved, but the key could not be checked: Acme answered HTTP 403 (Missing Authentication Token). If chats then fail to authenticate, replace the key.") as never),
    {
      text: "Saved, but the key could not be checked: Acme answered HTTP 403 (Missing Authentication Token). If chats then fail to authenticate, replace the key.",
      reason: "Acme answered HTTP 403 (Missing Authentication Token)",
    },
  );
  // A provider's own sentence may carry full stops of its own.
  assert.equal(
    keyWarning(saved("Saved, but the key could not be checked: Acme answered HTTP 400 (Key invalid. Try again.). If chats then fail to authenticate, replace the key.") as never)?.reason,
    "Acme answered HTTP 400 (Key invalid. Try again.)",
  );
  // An unknown shape is still shown, whole.
  assert.deepEqual(keyWarning({ title: "x", data: { warning: "Saved, but the gateway is odd." } } as never), {
    text: "Saved, but the gateway is odd.",
    reason: "",
  });
  // A server that only prints lines still gets its warning through.
  assert.equal(
    keyWarning({ title: "x", lines: ["Saved.", "Saved, but the key could not be checked: Haarness could not reach Acme. If chats then fail to authenticate, replace the key."] } as never)?.reason,
    "Haarness could not reach Acme",
  );
  assert.equal(keyWarning({ title: "Providers", lines: ["Saved. Runtime configuration changes apply when active work is idle."], data: {} } as never), null);
  assert.equal(keyWarning(undefined), null);
});
