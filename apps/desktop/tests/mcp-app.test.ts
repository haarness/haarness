import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import {
  INPUT,
  LEFT,
  REFUSED,
  STYLE_VARIABLES,
  themeTokens,
  WHEEL,
  createBridge,
  document,
  granted,
  linkScheme,
  messageText,
  payload,
  payloadFor,
  recall,
  remember,
  policy,
  resource,
  sources,
  variables,
  webLink,
  HOVER,
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
      "https://8.8.8.8",
      "'unsafe-eval'",
      "*",
      "https://x.example; script-src *",
      "data:",
      "javascript:alert(1)",
      42,
    ]),
    ["https://cdn.example.com", "https://*.example.com", "wss://live.example.com:8443/socket", "https://8.8.8.8"],
  );
  // Every host, plain http, and this computer or its network are refused.
  for (const host of [
    "https://*", "http://*", "wss://*", "https://*.com", "https://*.*", "https://cdn.*.example.com", "https://intranet",
    "http://cdn.example.com", "ws://live.example.com",
    "https://localhost", "https://localhost:3000", "https://app.localhost", "https://printer.local", "https://box.home.arpa",
    "https://127.0.0.1", "http://127.0.0.1:8080", "https://127.1.2.3", "https://0.0.0.0", "https://[::1]", "https://[fe80::1]",
    "https://10.0.0.5", "https://172.16.0.1", "https://172.31.255.255", "https://192.168.1.10", "https://169.254.169.254",
    "https://100.64.0.1", "https://224.0.0.1", "https://localhost.",
  ])
    assert.deepEqual(sources([host]), [], host);
  assert.deepEqual(sources(["https://172.32.0.1", "https://11.0.0.1"]), ["https://172.32.0.1", "https://11.0.0.1"]);
  // Hosts are compared as a browser reads them, and a wildcard may not cover a shared suffix.
  for (const host of [
    "https://0x7f.1", "https://127.1", "https://2130706433", "https://0177.0.0.1", "https://127.0.0.1.nip.io",
    "https://app.10.0.0.5.sslip.io", "https://192-168-1-10.sslip.io", "https://*.co.uk", "https://*.github.io",
    "https://*.vercel.app", "https://user:secret@cdn.example.com", "https://cdn.example.com/?key=1", "https://cdn.example.com/#x",
  ])
    assert.deepEqual(sources([host]), [], host);
  assert.deepEqual(
    sources(["https://CDN.Example.com/", "https://cdn.example.com:443/lib/", "https://*.example.co.uk", "https://my-app.github.io"]),
    ["https://cdn.example.com", "https://cdn.example.com/lib/", "https://*.example.co.uk", "https://my-app.github.io"],
  );
  assert.deepEqual(sources("https://cdn.example.com"), []);
  const closed = policy();
  for (const directive of [
    "default-src 'none'",
    "connect-src 'none'",
    "frame-src 'none'",
    "object-src 'none'",
    "form-action 'none'",
    "script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'",
    "worker-src blob:",
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
  assert.ok(open.includes("script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval' https://cdn.example.com;"));
  assert.ok(open.includes("frame-src 'none'"));
  assert.ok(!open.includes("'unsafe-eval'"));
  assert.ok(!open.includes("youtube"));
  assert.deepEqual(granted({ csp: { frameDomains: ["https://www.youtube.com"], connectDomains: ["https://a.example"] } }), {
    connectDomains: ["https://a.example"],
    resourceDomains: [],
    frameDomains: [],
    baseUriDomains: [],
  });
});

test("a declared host without a scheme is an https source, checked like one with a scheme", () => {
  assert.deepEqual(
    sources([
      "cdn.example.com",
      "*.example.com",
      "CDN.Example.org:8443/lib/",
      "cdn.example.net:443",
      "8.8.8.8",
      "self.example.net",
      "https://cdn.example.com",
      "wss://stream.example.net",
    ]),
    [
      "https://cdn.example.com",
      "https://*.example.com",
      "https://cdn.example.org:8443/lib/",
      "https://cdn.example.net",
      "https://8.8.8.8",
      "https://self.example.net",
      "wss://stream.example.net",
    ],
  );
  // What would be refused with a scheme is refused without one, and words
  // that are not hosts stay out.
  for (const host of [
    "localhost", "localhost:3000", "app.localhost", "printer.local", "127.0.0.1", "127.0.0.1:8080", "0x7f.1",
    "2130706433", "10.0.0.5", "192.168.1.10:3000", "127.0.0.1.nip.io", "*.co.uk", "*.github.io", "*.com", "*", "*.*",
    "intranet", "self", "none", "unsafe-inline", "strict-dynamic", "sha256-q1w2e3", "nonce-r4nd0m", "data:", "blob:",
    "javascript:alert(1)", "user@cdn.example.com", "user:secret@cdn.example.com", "user:1@cdn.example.com",
    "cdn.example.com:99999", "cdn.example.com:*", "cdn.example.com:", "*.", "cdn.example.com?key=1",
    "cdn.example.com/#x", "cdn.example.com/?key=1", ".example.com", "[::1]", "cdn.example.com https://mirror.example.org",
  ])
    assert.deepEqual(sources([host]), [], host);
  const meta = { csp: { connectDomains: ["data.example.org"], resourceDomains: ["*.static.example.net", "localhost:3000"] } };
  assert.ok(policy(meta).includes("connect-src https://data.example.org;"), policy(meta));
  assert.ok(policy(meta).includes("script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval' https://*.static.example.net;"), policy(meta));
  assert.deepEqual(granted(meta).connectDomains, ["https://data.example.org"]);
});

test("every page policy allows WebAssembly and blob: workers, and none allows eval", () => {
  for (const meta of [undefined, { csp: { resourceDomains: ["https://cdn.example.com", "blob:"], connectDomains: ["blob:"] } }]) {
    const value = policy(meta);
    assert.match(value, /(^|; )script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'( |;)/, value);
    assert.equal(value.match(/wasm-unsafe-eval/g)?.length, 1, value);
    assert.ok(value.split("; ").includes("worker-src blob:"), value);
    assert.ok(!value.includes("'unsafe-eval'"), value);
  }
  // Neither can be declared: a quoted keyword is no source.
  const declared = policy({ csp: { resourceDomains: ["'wasm-unsafe-eval'", "'unsafe-eval'"], connectDomains: ["worker-src"] } });
  assert.ok(!declared.includes("'unsafe-eval'"), declared);
  assert.ok(declared.includes("connect-src 'none'"), declared);
});

test("blob: and data: are taken for resources and connections only", () => {
  const meta = {
    csp: {
      resourceDomains: ["https://tiles.example.com", "blob:", "DATA:", "blob:"],
      connectDomains: ["https://tiles.example.com", "Blob:"],
      frameDomains: ["blob:", "data:", "https://www.example.com"],
      baseUriDomains: ["data:", "blob:"],
    },
  };
  const value = policy(meta);
  for (const directive of [
    "script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval' https://tiles.example.com blob: data:",
    "style-src 'self' 'unsafe-inline' https://tiles.example.com blob: data:",
    // Listed once each where the directive already has them.
    "img-src 'self' data: blob: https://tiles.example.com",
    "font-src 'self' data: https://tiles.example.com blob:",
    "media-src 'self' data: blob: https://tiles.example.com",
    "connect-src https://tiles.example.com blob:",
    "frame-src 'none'",
    "base-uri 'self'",
  ])
    assert.ok(value.split("; ").includes(directive), `${directive} in ${value}`);
  assert.deepEqual(granted(meta), {
    connectDomains: ["https://tiles.example.com", "blob:"],
    resourceDomains: ["https://tiles.example.com", "blob:", "data:"],
    frameDomains: [],
    baseUriDomains: [],
  });
  // Only the bare scheme: a blob or data URL, a wildcard after it, or another local scheme is no source.
  for (const item of [
    "blob:https://tiles.example.com/1f0c",
    "blob:null/1f0c",
    "data:text/javascript,alert(1)",
    "data:,x",
    "blob:*.example.com",
    "data:;default-src *",
    "blob:'unsafe-eval'",
    "blob",
    "data",
    "filesystem:https://tiles.example.com/temporary/x",
    "javascript:",
    "about:",
    "ws:",
  ])
    assert.deepEqual(sources([item], true), [], item);
  assert.deepEqual(sources(["blob:", "data:"]), []);
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
  // The person's presses are reported with the token: trusted events only, and the script leaves the page first.
  assert.ok(html.includes(INPUT) && html.includes("e.isTrusted&&!e.repeat"));
  assert.ok(html.includes('addEventListener("pointerdown",press,true);addEventListener("keydown",press,true)'));
  // The pointer coming into the page and leaving it is reported once each way, with the token, for the page's options.
  assert.ok(html.includes(HOVER) && html.includes("if(!e.relatedTarget)hover(0)"));
  assert.ok(html.includes(`p.postMessage({${HOVER}:[v,s]},"*")`));
  assert.ok(html.includes("document.currentScript.remove()"));
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

test("every Haarness token a theme variable comes from tells whether the theme moved", () => {
  const tokens = themeTokens();
  assert.equal(new Set(tokens).size, tokens.length);
  const sources = Object.values(STYLE_VARIABLES).flatMap((source) => ("token" in source ? [source.token] : []));
  assert.deepEqual([...new Set(sources)].sort(), [...tokens].sort());
  // Fonts and borders, not only colours.
  for (const token of ["--sans", "--mono", "--r-md", "--line", "--shadow-1", "--font-size"]) assert.ok(tokens.includes(token), token);
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
    openLink: async (url, gesture) => (calls.push(gesture ? ["openLink", url, "gesture"] : ["openLink", url]), true),
    compose: (text, gesture) => (calls.push(gesture ? ["compose", text, "gesture"] : ["compose", text]), true),
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

test("the bridge says whether the person had just acted in the page with each link and message", async () => {
  const { value, calls } = host();
  const posted: Record<string, unknown>[] = [];
  const bridge = createBridge(value, (message) => posted.push(message));
  await bridge.receive({ jsonrpc: "2.0", id: 1, method: "ui/open-link", params: { url: "https://example.com/a" } }, true);
  await bridge.receive({ jsonrpc: "2.0", id: 2, method: "ui/open-link", params: { url: "https://example.com/b" } });
  await bridge.receive({ jsonrpc: "2.0", id: 3, method: "ui/message", params: { content: [{ type: "text", text: "hi" }] } }, true);
  assert.deepEqual(calls, [
    ["openLink", "https://example.com/a", "gesture"],
    ["openLink", "https://example.com/b"],
    ["compose", "hi", "gesture"],
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

test("a refused link reaches the host too, so the person who clicked it can be told", async () => {
  const { value, calls } = host({
    refusedLink: (url, gesture) => void calls.push(gesture ? ["refusedLink", url, "gesture"] : ["refusedLink", url]),
  });
  const posted: Record<string, unknown>[] = [];
  const bridge = createBridge(value, (message) => posted.push(message));
  await bridge.receive({ jsonrpc: "2.0", id: 1, method: "ui/open-link", params: { url: "file:///etc/hosts" } }, true);
  await bridge.receive({ jsonrpc: "2.0", id: 2, method: "ui/open-link", params: { url: 42 } });
  await bridge.receive({ jsonrpc: "2.0", id: 3, method: "ui/open-link", params: { url: "https://example.com/ok" } });
  assert.deepEqual(calls, [
    ["refusedLink", "file:///etc/hosts", "gesture"],
    ["refusedLink", ""],
    ["openLink", "https://example.com/ok"],
  ]);
  assert.deepEqual(posted.slice(0, 2), [
    { jsonrpc: "2.0", id: 1, error: { code: -32602, message: REFUSED.link } },
    { jsonrpc: "2.0", id: 2, error: { code: -32602, message: REFUSED.link } },
  ]);
});

test("a refused link is named only by its scheme, never by the page's own text", () => {
  assert.equal(linkScheme("file:///etc/hosts"), "file:");
  assert.equal(linkScheme(" JavaScript:alert(1)"), "javascript:");
  assert.equal(linkScheme("SMB://server/share"), "smb:");
  // Any other scheme could be words the page chose: the notice names none.
  assert.equal(linkScheme("vscode://open?x=1"), undefined);
  assert.equal(linkScheme("sign-in.at.haarness-help.com:443"), undefined);
  // Nor a worded address, or text with no scheme at all.
  assert.equal(linkScheme("Haarness: sign in at https://evil.example"), undefined);
  assert.equal(linkScheme("Sign in at https://evil.example"), undefined);
  assert.equal(linkScheme("relative/path"), undefined);
  assert.equal(linkScheme(""), undefined);
  assert.equal(linkScheme("a".repeat(40) + ":x"), undefined);
});

test("a message waits for the host's answer: inserted, or declined", async () => {
  let answer!: (inserted: boolean) => void;
  const { value } = host({ compose: () => new Promise<boolean>((resolve) => (answer = resolve)) });
  const posted: Record<string, unknown>[] = [];
  const bridge = createBridge(value, (message) => posted.push(message));
  const pending = bridge.receive({ jsonrpc: "2.0", id: 1, method: "ui/message", params: { role: "user", content: [{ type: "text", text: "hi" }] } });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual(posted, []);
  answer(false);
  await pending;
  assert.deepEqual(posted, [{ jsonrpc: "2.0", id: 1, result: { isError: true } }]);
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
  for (const directive of [
    "default-src 'none'",
    "frame-src 'none'",
    "object-src 'none'",
    "form-action 'none'",
    "base-uri 'none'",
    "connect-src blob: data: https: wss:",
    // The page inherits this policy, and a worker it starts inherits both: WebAssembly and blob:
    // workers must pass here as well as in the page's own policy.
    "script-src 'unsafe-inline' 'wasm-unsafe-eval' blob: data: https:",
    "worker-src blob:",
  ])
    assert.ok(csp.split("; ").includes(directive), directive);
  // Secure origins and what the page makes itself only, even before the page's own policy narrows them.
  assert.ok(!/\b(http|ws):/.test(csp), csp);
  assert.ok(!csp.includes("'unsafe-eval'"), csp);
  assert.ok(!/(^|\s)\*(\s|;|$)/.test(csp), csp);
  // The page frame runs scripts and forms only: an opaque origin, no popups, no top navigation.
  assert.match(page, /setAttribute\('sandbox', 'allow-scripts allow-forms'\)/);
  // Each relayed message says whether the person had just acted in the page, read as it arrives.
  assert.match(page, /navigator\.userActivation\.isActive\) && document\.activeElement === frame/);
  assert.match(page, /type: 'receive', message: data, active \}/);
  assert.ok(!page.includes("allow-same-origin"));
  const view = await readFile(new URL("../src/renderer/McpAppView.tsx", import.meta.url), "utf8");
  assert.match(view, /sandbox="allow-scripts allow-forms"/);
  assert.ok(!view.includes("allow-same-origin"));
});

test("the page and height caches keep only their most recent entries", () => {
  const map = new Map<string, number>();
  for (let index = 0; index < 5; index++) remember(map, `page ${index}`, index, 3);
  assert.deepEqual([...map.keys()], ["page 2", "page 3", "page 4"]);
  // Reading an entry keeps it; the oldest unread one goes next.
  assert.equal(recall(map, "page 2"), 2);
  remember(map, "page 5", 5, 3);
  assert.deepEqual([...map.keys()], ["page 4", "page 2", "page 5"]);
  remember(map, "page 4", 40, 3);
  assert.deepEqual([...map.entries()], [["page 2", 2], ["page 5", 5], ["page 4", 40]]);
  assert.equal(recall(map, "gone"), undefined);
});
