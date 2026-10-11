import test from "node:test";
import assert from "node:assert/strict";
import {
  browserSiteAccess,
  destinationOrigin,
  navigationResources,
  normalizeURL,
  wwwTwin,
} from "../src/policy";

test("URLs normalise to HTTP(S) or about:blank", () => {
  assert.equal(normalizeURL("example.test/a"), "https://example.test/a");
  assert.equal(normalizeURL("localhost:3000"), "http://localhost:3000");
  assert.equal(normalizeURL(" "), "about:blank");
  assert.throws(() => normalizeURL("file:///etc/passwd"), /Only HTTP, HTTPS, and about:blank/);
  assert.equal(destinationOrigin("https://user:pw@example.test/"), undefined);
});

test("one approval covers a site with and without www", () => {
  assert.equal(wwwTwin("https://example.com"), "https://www.example.com");
  assert.equal(wwwTwin("https://www.example.com:8443"), "https://example.com:8443");
  assert.equal(wwwTwin("https://example.com/a?b"), "https://www.example.com/a?b");
  for (const none of ["http://localhost:3000", "https://www.com", "http://127.0.0.1", "http://[::1]:80", "about:blank", "ftp://example.com"])
    assert.equal(wwwTwin(none), undefined, none);
  assert.deepEqual(navigationResources("example.com/path"), ["https://example.com/path"]);
  assert.deepEqual(navigationResources("example.com/path", { origins: true, twin: true }), [
    "https://example.com",
    "https://www.example.com",
  ]);
  assert.deepEqual(navigationResources("about:blank", { origins: true, twin: true }), ["about:blank"]);
});

test("site rules prefer exact origins, then the most specific wildcard, for any categories", () => {
  type Decision = "allow" | "ask" | "deny";
  const ask: Record<"browsing" | "downloads" | "uploads" | "scripts", Decision> = { browsing: "ask", downloads: "ask", uploads: "ask", scripts: "ask" };
  const policy = {
    defaults: ask,
    sites: [
      { origin: "https://*.example.test", ...ask, browsing: "allow" as const },
      { origin: "https://*.a.example.test", ...ask, scripts: "deny" as const },
      { origin: "https://a.example.test", ...ask, uploads: "deny" as const },
    ],
  };
  assert.equal(browserSiteAccess(policy, "https://a.example.test").uploads, "deny");
  assert.equal(browserSiteAccess(policy, "https://b.a.example.test").scripts, "deny");
  assert.equal(browserSiteAccess(policy, "https://b.example.test").browsing, "allow");
  assert.equal(browserSiteAccess(policy, "https://example.test"), ask);
});
