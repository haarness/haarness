import assert from "node:assert/strict";
import test from "node:test";
import type { Policy } from "../src/core/browser";
import { blockedMessage, decideRequest, fetchPatterns, landing } from "../src/core/fetch-policy";
import { allowedOnBlank, normalizeURL, restricted, siteAccess, supportedInChrome, wwwTwin } from "../src/core/policy";

const policy: Policy = {
  enabled: true,
  defaults: { browsing: "ask", downloads: "ask", uploads: "ask" },
  sites: [
    { origin: "https://evil.example", browsing: "deny", downloads: "deny", uploads: "deny" },
    { origin: "https://*.trusted.example", browsing: "allow", downloads: "ask", uploads: "ask" },
  ],
};
const context = { policy, approved: new Set(["https://approved.example"]) };
const page = (url: string) => decideRequest(context, { url, resourceType: "Document", mainFrame: true });

test("a blocked origin is refused for every kind of request", () => {
  assert.deepEqual(page("https://evil.example/login"), { action: "block", reason: "deny", origin: "https://evil.example" });
  assert.equal(decideRequest(context, { url: "https://evil.example/x.js", resourceType: "Script", mainFrame: false }).action, "block");
  assert.deepEqual(fetchPatterns(policy), [{ resourceType: "Document" }, { urlPattern: "https://evil.example/*" }]);
});

test("the tab's page may load an Ask origin only once this chat approved it", () => {
  assert.equal(page("https://approved.example/next").action, "continue");
  assert.deepEqual(page("https://other.example/"), { action: "block", reason: "ask", origin: "https://other.example" });
  // Frames and subresources of other origins are not the page; they continue.
  assert.equal(decideRequest(context, { url: "https://other.example/", resourceType: "Document", mainFrame: false }).action, "continue");
  assert.equal(decideRequest(context, { url: "https://cdn.other.example/a.css", resourceType: "Stylesheet", mainFrame: true }).action, "continue");
  // Allow rules need no approval, including wildcard subdomains.
  assert.equal(page("https://app.trusted.example/").action, "continue");
});

test("each redirect hop is decided on its own", () => {
  const hops = ["https://approved.example/start", "https://approved.example/sso", "https://other.example/landing"];
  assert.deepEqual(hops.map((url) => page(url).action), ["continue", "continue", "block"]);
});

test("the page may not leave the web", () => {
  assert.deepEqual(page("file:///etc/passwd"), { action: "block", reason: "scheme", origin: "file:" });
  assert.deepEqual(page("chrome://settings"), { action: "block", reason: "scheme", origin: "chrome:" });
  assert.equal(decideRequest(context, { url: "data:image/png;base64,AA", resourceType: "Image", mainFrame: true }).action, "continue");
  assert.equal(landing(context, "about:blank").action, "continue");
  assert.equal(landing(context, "chrome-error://chromewebdata/").action, "continue", "a failed load is reported, not blocked");
  assert.equal(landing(context, "https://other.example/").action, "block");
});

test("turning agents off, or blocking every site by default, pauses every request", () => {
  assert.deepEqual(fetchPatterns({ ...policy, enabled: false }), [{ urlPattern: "*" }]);
  assert.deepEqual(fetchPatterns({ ...policy, defaults: { ...policy.defaults, browsing: "deny" } }), [{ urlPattern: "*" }]);
  assert.equal(decideRequest({ ...context, policy: { ...policy, enabled: false } }, { url: "https://approved.example/", resourceType: "Image", mainFrame: false }).action, "block");
});

test("block messages name the origin and say what the agent can do", () => {
  const ask = page("https://other.example/");
  assert.equal(ask.action, "block");
  if (ask.action === "block") assert.match(blockedMessage(ask), /https:\/\/other\.example.*Call browser_navigate with that URL to ask the user, or choose another site\./);
});

test("www twins, restricted pages and Chrome-only limits", () => {
  assert.equal(wwwTwin("https://example.com"), "https://www.example.com");
  assert.equal(wwwTwin("https://www.example.com:8443"), "https://example.com:8443");
  for (const none of ["http://localhost:3000", "http://127.0.0.1:8080", "http://intranet", "https://www.com"]) assert.equal(wwwTwin(none), undefined, none);
  for (const url of ["chrome://extensions", "chrome-extension://abc/welcome.html", "devtools://devtools/x", "file:///tmp/a", "view-source:https://a.example", "data:text/html,hi"])
    assert.equal(restricted(url), true, url);
  for (const url of ["about:blank", "https://a.example/", "http://localhost:3000/"]) assert.equal(restricted(url), false, url);
  assert.equal(allowedOnBlank("snapshot"), true);
  assert.equal(allowedOnBlank("evaluate"), false);
  assert.equal(allowedOnBlank("click"), false);
  for (const method of ["lighthouse", "heap.snapshot", "cpu.start", "trace.start"] as const) assert.equal(supportedInChrome(method), false);
  assert.equal(supportedInChrome("click"), true);
  assert.equal(normalizeURL("example.com"), "https://example.com");
  assert.equal(normalizeURL("localhost:3000/x"), "http://localhost:3000/x");
  assert.throws(() => normalizeURL("file:///etc"), /Only HTTP/);
  assert.deepEqual(siteAccess(policy, "https://a.b.trusted.example").browsing, "allow");
});
