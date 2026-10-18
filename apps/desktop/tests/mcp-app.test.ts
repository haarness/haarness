import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import {
  LEFT,
  REFUSED,
  STYLE_VARIABLES,
  WHEEL,
  createBridge,
  document,
  granted,
  messageText,
  payload,
  payloadFor,
  policy,
  resource,
  sources,
  variables,
  webLink,
  type Host,
} from "../src/renderer/mcp-app";

const RESULT = {
  content: [{ type: "text", text: "Lisbon: sunny" }],
  structuredContent: { city: "Lisbon" },
};

test("a tool result names its page only through a well-formed mcp_app payload", () => {
  const metadata = {
    city: "Lisbon",
    mcp_app: {
      server: "weather",
      uri: "ui://weather/view.html",
      tool: { name: "show", title: "Show", inputSchema: { type: "object" } },
      result: RESULT,
    },
  };
  const value = payload(metadata)!;
  assert.equal(value.server, "weather");
  assert.equal(value.uri, "ui://weather/view.html");
  assert.deepEqual(value.tool, { name: "show", title: "Show", inputSchema: { type: "object" } });
  // The stored result itself, so an unchanged result compares equal.
  assert.equal(value.result, metadata.mcp_app.result);
  assert.equal(value.tooLarge, false);
  const large = payload({ mcp_app: { server: "weather", uri: "ui://weather/view.html", result_too_large: true } })!;
  assert.equal(large.result, undefined);
  assert.equal(large.tooLarge, true);
  for (const bad of [
    undefined,
    null,
    "mcp_app",
    { mcp_app: null },
    { mcp_app: { server: "", uri: "ui://x" } },
    { mcp_app: { server: "x", uri: "https://example.com/page.html" } },
    { mcp_app: { server: 1, uri: "ui://x" } },
    { structuredContent: { mcp_app: true } },
  ])
    assert.equal(payload(bad), undefined, JSON.stringify(bad));
});

test("only the payload server's own paired tool may name a page", () => {
  const metadata = {
    mcp_app: { server: "weather", uri: "ui://weather/view.html", tool: { name: "show" }, result: RESULT },
  };
  assert.equal(payloadFor("mcp__weather__show", metadata)?.server, "weather");
  // Another server's tool, or another tool of this server, naming the page.
  for (const name of ["mcp__evil__show", "mcp__evil__forge", "mcp__weather__plain", "show", "mcp__weather__show__x"])
    assert.equal(payloadFor(name, metadata), undefined, name);
  assert.equal(payloadFor("mcp__weather__show", { mcp_app: { ...metadata.mcp_app, tool: {} } }), undefined);
  // Names written with characters a tool name cannot hold compare cleaned.
  const dotted = { mcp_app: { ...metadata.mcp_app, server: "my.server", tool: { name: "show it" } } };
  assert.equal(payloadFor("mcp__my_server__show_it", dotted)?.server, "my.server");
  assert.equal(payloadFor("mcp__my.server__show it", dotted)?.server, "my.server");
});

test("the page is the HTML content of its resource, with its own ui metadata", () => {
  const meta = { csp: { connectDomains: ["https://api.example.com"] } };
  const read = {
    contents: [
      { uri: "ui://w/other.html", mimeType: "text/html", text: "<p>other</p>" },
      { uri: "ui://w/view.html", mimeType: "text/html;profile=mcp-app", text: "<p>view</p>", _meta: { ui: meta } },
    ],
  };
  assert.deepEqual(resource(read, "ui://w/view.html"), { html: "<p>view</p>", meta });
  assert.deepEqual(resource(read, "ui://w/missing.html"), { html: "<p>other</p>", meta: {} });
  const blob = { contents: [{ uri: "ui://w/b.html", mimeType: "text/html", blob: Buffer.from("<p>é</p>").toString("base64") }] };
  assert.equal(resource(blob, "ui://w/b.html")?.html, "<p>é</p>");
  assert.equal(resource({ contents: [] }, "ui://w/view.html"), undefined);
  assert.equal(resource({}, "ui://w/view.html"), undefined);
  assert.equal(resource({ contents: [{ uri: "ui://w/view.html", mimeType: "text/html" }] }, "ui://w/view.html"), undefined);
});

test("only host sources reach the page's policy, and no network is the default", () => {
  assert.deepEqual(
    sources([
      "https://cdn.example.com",
      "https://*.example.com",
      "wss://live.example.com:8443/socket",
      "http://localhost:3000",
      "'unsafe-eval'",
      "*",
      "https://x.example; script-src *",
      "data:",
      "javascript:alert(1)",
      42,
    ]),
    ["https://cdn.example.com", "https://*.example.com", "wss://live.example.com:8443/socket", "http://localhost:3000"],
  );
  assert.deepEqual(sources("https://cdn.example.com"), []);
  const closed = policy();
  for (const directive of [
    "default-src 'none'",
    "connect-src 'none'",
    "frame-src 'none'",
    "object-src 'none'",
    "form-action 'none'",
    "script-src 'self' 'unsafe-inline'",
  ])
    assert.ok(closed.split("; ").includes(directive), `${directive} in ${closed}`);
  const open = policy({
    csp: {
      connectDomains: ["https://api.example.com", "'self' *"],
      resourceDomains: ["https://cdn.example.com", "'unsafe-eval'"],
      frameDomains: ["https://www.youtube.com"],
    },
  });
  assert.ok(open.includes("connect-src https://api.example.com;"));
  assert.ok(open.includes("script-src 'self' 'unsafe-inline' https://cdn.example.com;"));
  assert.ok(open.includes("frame-src 'none'"));
  assert.ok(!open.includes("unsafe-eval"));
  assert.ok(!open.includes("youtube"));
  assert.deepEqual(granted({ csp: { frameDomains: ["https://www.youtube.com"], connectDomains: ["https://a.example"] } }), {
    connectDomains: ["https://a.example"],
    resourceDomains: [],
    frameDomains: [],
    baseUriDomains: [],
  });
});

test("the page document starts with its policy and the host script, after its doctype", () => {
  const html = document('<!DOCTYPE html><html><head><title>x</title></head><body><script>run()</script></body></html>', {
    csp: { connectDomains: ['https://a.example"><script>alert(1)</script>'] },
  }, 'token"</script>');
  assert.ok(html.startsWith('<!DOCTYPE html><meta http-equiv="Content-Security-Policy" content="default-src'));
  // The policy and the host script come before anything the server wrote.
  assert.ok(html.indexOf("Content-Security-Policy") < html.indexOf("<title>"));
  assert.ok(html.indexOf(WHEEL) < html.indexOf("run()"));
  assert.ok(html.includes(LEFT));
  // Neither a declared domain nor the token can end the tags they are written into.
  assert.ok(!html.includes('"><script>alert(1)'));
  assert.ok(!html.includes('token"</script>'));
  assert.ok(document("<p>bare</p>", undefined, "t").startsWith("<!doctype html><meta"));
});

test("theme variables come from Haarness tokens, with colours resolved", () => {
  const read = (token: string) => (token === "--sans" ? " Inter, sans-serif " : token === "--r-md" ? "9px" : "");
  const color = (token: string) => `rgb(${token.length}, 0, 0)`;
  const values = variables(read, color);
  assert.equal(values["--font-sans"], "Inter, sans-serif");
  assert.equal(values["--border-radius-md"], "9px");
  assert.equal(values["--color-background-primary"], `rgb(${"--surface".length}, 0, 0)`);
  assert.equal(values["--font-weight-bold"], "700");
  // Tokens that read as empty are left out rather than sent blank.
  assert.equal(values["--font-mono"], undefined);
  for (const name of Object.keys(STYLE_VARIABLES)) assert.match(name, /^--(color|font|border|shadow)-/);
});

test("a page may open http, https and mailto links and send text messages only", () => {
  assert.equal(webLink("https://example.com/a?b=1"), "https://example.com/a?b=1");
  assert.equal(webLink("mailto:me@example.com"), "mailto:me@example.com");
  for (const bad of ["javascript:alert(1)", "file:///etc/passwd", "haarness://open?x", "data:text/html,x", "/relative", "", 7, "x".repeat(9000)])
    assert.equal(webLink(bad), undefined, String(bad).slice(0, 40));
  assert.equal(
    messageText({ role: "user", content: [{ type: "text", text: "First" }, { type: "text", text: "Second " }] }),
    "First\n\nSecond",
  );
  for (const bad of [
    {},
    { role: "assistant", content: [{ type: "text", text: "x" }] },
    { role: "user", content: [] },
    { role: "user", content: [{ type: "image", data: "x", mimeType: "image/png" }] },
    { role: "user", content: [{ type: "text", text: "   " }] },
    { role: "user", content: [{ type: "text", text: "x".repeat(40000) }] },
  ])
    assert.equal(messageText(bad), undefined, JSON.stringify(bad).slice(0, 60));
});

function host(overrides: Partial<Host> = {}) {
  const calls: unknown[][] = [];
  const value: Host = {
    initialize: (params) => (calls.push(["initialize", params]), { protocolVersion: "2026-01-26" }),
    initialized: () => void calls.push(["initialized"]),
    resize: (size) => void calls.push(["resize", size]),
    openLink: async (url) => (calls.push(["openLink", url]), true),
    compose: (text) => (calls.push(["compose", text]), true),
    readResource: async (uri) => (calls.push(["readResource", uri]), { contents: [] }),
    ...overrides,
  };
  return { value, calls };
}

async function exchange(bridgeHost: Host, messages: unknown[]) {
  const posted: Record<string, unknown>[] = [];
  const bridge = createBridge(bridgeHost, (message) => posted.push(message));
  for (const message of messages) await bridge.receive(message);
  return { posted, bridge };
}

test("the bridge answers the page's requests and passes its notifications to the host", async () => {
  const { value, calls } = host();
  const { posted } = await exchange(value, [
    { jsonrpc: "2.0", id: 1, method: "ui/initialize", params: { protocolVersion: "2026-01-26" } },
    { jsonrpc: "2.0", method: "ui/notifications/initialized" },
    { jsonrpc: "2.0", method: "ui/notifications/size-changed", params: { height: 240, width: "wide" } },
    { jsonrpc: "2.0", id: 2, method: "ui/open-link", params: { url: "https://example.com/forecast" } },
    { jsonrpc: "2.0", id: 3, method: "ui/message", params: { role: "user", content: [{ type: "text", text: "Tomorrow?" }] } },
    { jsonrpc: "2.0", id: "four", method: "resources/read", params: { uri: "ui://weather/view.html" } },
    { jsonrpc: "2.0", id: 5, method: "ui/request-display-mode", params: { mode: "fullscreen" } },
    { jsonrpc: "2.0", id: 6, method: "ping" },
    // Not JSON-RPC 2.0, or not a message at all: ignored.
    { id: 7, method: "ui/open-link", params: { url: "https://example.com" } },
    "ui/open-link",
    null,
  ]);
  assert.deepEqual(calls, [
    ["initialize", { protocolVersion: "2026-01-26" }],
    ["initialized"],
    ["resize", { width: undefined, height: 240 }],
    ["openLink", "https://example.com/forecast"],
    ["compose", "Tomorrow?"],
    ["readResource", "ui://weather/view.html"],
  ]);
  assert.deepEqual(posted, [
    { jsonrpc: "2.0", id: 1, result: { protocolVersion: "2026-01-26" } },
    { jsonrpc: "2.0", id: 2, result: {} },
    { jsonrpc: "2.0", id: 3, result: {} },
    { jsonrpc: "2.0", id: "four", result: { contents: [] } },
    { jsonrpc: "2.0", id: 5, result: { mode: "inline" } },
    { jsonrpc: "2.0", id: 6, result: {} },
  ]);
});

test("the bridge refuses tool calls, context updates, downloads, other resources and other links", async () => {
  const { value, calls } = host({ openLink: async () => false, compose: () => false });
  const { posted } = await exchange(value, [
    { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "refresh", arguments: {} } },
    { jsonrpc: "2.0", id: 2, method: "ui/update-model-context", params: { content: [] } },
    { jsonrpc: "2.0", id: 3, method: "ui/download-file", params: { contents: [] } },
    { jsonrpc: "2.0", id: 4, method: "resources/read", params: { uri: "file:///etc/hosts" } },
    { jsonrpc: "2.0", id: 5, method: "ui/open-link", params: { url: "javascript:alert(1)" } },
    { jsonrpc: "2.0", id: 6, method: "ui/message", params: { role: "user", content: [{ type: "image" }] } },
    { jsonrpc: "2.0", id: 7, method: "sampling/createMessage", params: {} },
    // Declined by the person, or a chat that cannot take a message.
    { jsonrpc: "2.0", id: 8, method: "ui/open-link", params: { url: "https://example.com" } },
    { jsonrpc: "2.0", id: 9, method: "ui/message", params: { content: [{ type: "text", text: "hi" }] } },
  ]);
  assert.deepEqual(calls, []);
  assert.deepEqual(posted, [
    { jsonrpc: "2.0", id: 1, error: { code: -32601, message: REFUSED.tools } },
    { jsonrpc: "2.0", id: 2, error: { code: -32601, message: REFUSED.context } },
    { jsonrpc: "2.0", id: 3, error: { code: -32601, message: REFUSED.download } },
    { jsonrpc: "2.0", id: 4, error: { code: -32602, message: REFUSED.read } },
    { jsonrpc: "2.0", id: 5, error: { code: -32602, message: REFUSED.link } },
    { jsonrpc: "2.0", id: 6, error: { code: -32602, message: REFUSED.message } },
    { jsonrpc: "2.0", id: 7, error: { code: -32601, message: "Method not found: sampling/createMessage" } },
    { jsonrpc: "2.0", id: 8, result: { isError: true } },
    { jsonrpc: "2.0", id: 9, result: { isError: true } },
  ]);
});

test("a failing host answers with an error, and the host's own requests get their answers", async () => {
  const { value } = host({ readResource: async () => { throw new Error("MCP server weather is not connected"); } });
  const { posted, bridge } = await exchange(value, [
    { jsonrpc: "2.0", id: 1, method: "resources/read", params: { uri: "ui://weather/view.html" } },
  ]);
  assert.deepEqual(posted.at(-1), {
    jsonrpc: "2.0",
    id: 1,
    error: { code: -32603, message: "MCP server weather is not connected" },
  });
  bridge.notify("ui/notifications/tool-result", RESULT);
  assert.deepEqual(posted.at(-1), { jsonrpc: "2.0", method: "ui/notifications/tool-result", params: RESULT });
  const teardown = bridge.request("ui/resource-teardown");
  const sent = posted.at(-1)!;
  assert.equal(sent.method, "ui/resource-teardown");
  await bridge.receive({ jsonrpc: "2.0", id: sent.id, result: { done: true } });
  assert.deepEqual(await teardown, { done: true });
});

test("the frame's host page is a separate document whose policy keeps the page in its frame", async () => {
  const page = await readFile(new URL("../src/renderer/public/mcp-app.html", import.meta.url), "utf8");
  const csp = page.match(/http-equiv="Content-Security-Policy" content="([^"]+)"/)?.[1] ?? "";
  for (const directive of ["default-src 'none'", "frame-src 'none'", "object-src 'none'", "form-action 'none'", "base-uri 'none'"])
    assert.ok(csp.split("; ").includes(directive), directive);
  // The page frame runs scripts and forms only: an opaque origin, no popups, no top navigation.
  assert.match(page, /setAttribute\('sandbox', 'allow-scripts allow-forms'\)/);
  assert.ok(!page.includes("allow-same-origin"));
  const view = await readFile(new URL("../src/renderer/McpAppView.tsx", import.meta.url), "utf8");
  assert.match(view, /sandbox="allow-scripts allow-forms"/);
  assert.ok(!view.includes("allow-same-origin"));
});
