import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import type { Decision, Policy } from "../src/core/browser";
import { blockedMessage, decideRequest, fetchPatterns, landing } from "../src/core/fetch-policy";
import { allowedOnBlank, normalizeURL, restricted, siteAccess, supportedInChrome, wwwTwin } from "../src/core/policy";

/** The site cases the server and Desktop run too (packages/browser-driver/tests). */
const cases = JSON.parse(
  readFileSync(new URL("../../../packages/browser-driver/tests/site-rules.cases.json", import.meta.url), "utf8"),
) as {
  policy: { defaults: Decision; sites: { origin: string; browsing: Decision }[] };
  browsing: Record<Decision, string[]>;
  userinfo: { url: string; username: string; password: string; origin: string }[];
  network: Record<string, string>;
  other: string[];
};

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
  assert.deepEqual(fetchPatterns(policy), [
    { resourceType: "Document" },
    { urlPattern: "https://evil.example/*" },
    { urlPattern: "https://evil.example./*" },
  ]);
});

test("shared site cases: a blocked site is refused however its host is spelled, for pages and subresources", () => {
  const access = (browsing: Decision) => ({ browsing, downloads: browsing, uploads: browsing });
  const shared: Policy = {
    enabled: true,
    defaults: access(cases.policy.defaults),
    sites: cases.policy.sites.map((site) => ({ origin: site.origin, ...access(site.browsing) })),
  };
  const context = { policy: shared, approved: new Set<string>() };
  for (const decision of ["deny", "ask", "allow"] as const)
    for (const url of cases.browsing[decision]) {
      const resource = decideRequest(context, { url, resourceType: "Script", mainFrame: false });
      const document = decideRequest(context, { url, resourceType: "Document", mainFrame: true });
      assert.equal(resource.action, decision === "deny" ? "block" : "continue", url);
      assert.equal(document.action, decision === "allow" ? "continue" : "block", url);
    }
  // WebSockets and other network schemes read as their HTTP(S) origin.
  for (const [url, origin] of Object.entries(cases.network)) {
    const decision = decideRequest(context, { url, resourceType: "WebSocket", mainFrame: false });
    const browsing = siteAccess(shared, origin).browsing;
    assert.equal(decision.action, browsing === "deny" ? "block" : "continue", url);
  }
  // Addresses with credentials are refused, even on an allowed site.
  for (const item of cases.userinfo) {
    const url = new URL(item.url);
    url.username = item.username;
    url.password = item.password;
    assert.deepEqual(
      decideRequest(context, { url: url.href, resourceType: "Image", mainFrame: false }),
      { action: "block", reason: "credentials", origin: item.origin },
      url.href,
    );
  }
  for (const url of ["data:text/html,hi", "blob:https://blocked.example/0123", "about:srcdoc"])
    assert.equal(decideRequest(context, { url, resourceType: "Image", mainFrame: false }).action, "continue", url);
  // Chrome keeps the trailing dot in a request URL, so the pause patterns name both spellings.
  assert.deepEqual(
    fetchPatterns({
      ...shared,
      sites: [
        { origin: "https://*.evil.example", ...access("deny") },
        { origin: "http://localhost:3000", ...access("deny") },
        { origin: "http://127.0.0.1:3000", ...access("deny") },
        { origin: "http://[::1]:3000", ...access("deny") },
      ],
    }).map((pattern) => pattern.urlPattern ?? pattern.resourceType),
    [
      "Document",
      "https://*.evil.example/*",
      "https://*.evil.example./*",
      "http://localhost:3000/*",
      "http://localhost.:3000/*",
      "http://127.0.0.1:3000/*",
      "http://[::1]:3000/*",
    ],
  );
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
  assert.match(
    blockedMessage({ action: "block", reason: "credentials", origin: "https://evil.example" }),
    /https:\/\/evil\.example that carries a user name or password/,
  );
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
