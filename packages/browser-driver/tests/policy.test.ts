import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  browserSiteAccess,
  carriedIPv4,
  carryingIPv6,
  destinationOrigin,
  navigationResources,
  normalizeURL,
  policyOrigin,
  wwwTwin,
} from "../src/policy";

type Decision = "allow" | "ask" | "deny";
/** The cases the server runs too (crates/haarness-server/src/browser_policy.rs). */
const siteCases = JSON.parse(
  readFileSync(new URL("site-rules.cases.json", import.meta.url), "utf8"),
) as {
  policy: { defaults: Decision; sites: { origin: string; browsing: Decision }[] };
  browsing: Record<Decision, string[]>;
  userinfo: { url: string; username: string; password: string; origin: string }[];
  network: Record<string, string>;
  other: string[];
};
/** Credentials are added at run time so the fixture holds no credential URL. */
const userinfoURLs = siteCases.userinfo.map((item) => {
  const url = new URL(item.url);
  url.username = item.username;
  url.password = item.password;
  return { url: url.href, origin: item.origin };
});
const casePolicy = {
  defaults: { browsing: siteCases.policy.defaults },
  sites: siteCases.policy.sites,
};

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

test("shared site cases decide alike in every browser, trailing dots included", () => {
  let checked = 0;
  for (const decision of ["deny", "ask", "allow"] as const)
    for (const url of siteCases.browsing[decision]) {
      const origin = destinationOrigin(url);
      assert.ok(origin, url);
      assert.equal(browserSiteAccess(casePolicy, origin).browsing, decision, url);
      // A URL reads as its origin, whichever the caller hands over.
      assert.equal(browserSiteAccess(casePolicy, url).browsing, decision, url);
      checked++;
    }
  assert.ok(checked > 30);
  assert.equal(destinationOrigin("https://blocked.example./x"), "https://blocked.example");
  assert.equal(destinationOrigin("http://localhost.:3000/"), "http://localhost:3000");
  // The address itself keeps its spelling; only the site it names is read.
  assert.equal(normalizeURL("https://blocked.example./x"), "https://blocked.example./x");
});

test("request filters read WebSockets by their HTTP(S) origin and refuse credentials", () => {
  for (const [url, origin] of Object.entries(siteCases.network))
    assert.deepEqual(policyOrigin(url), { origin, credentials: false }, url);
  for (const { url, origin } of userinfoURLs) {
    assert.deepEqual(policyOrigin(url), { origin, credentials: true }, url);
    // Never a destination: navigation refuses it.
    assert.equal(destinationOrigin(url), undefined, url);
  }
  const socket = new URL("wss://blocked.example./socket");
  socket.username = "user";
  assert.deepEqual(policyOrigin(socket.href), { origin: "https://blocked.example", credentials: true });
  for (const url of siteCases.other) assert.equal(policyOrigin(url), undefined, url);
  assert.equal(policyOrigin("not a url"), undefined);
  assert.equal(destinationOrigin("wss://open.example/"), undefined);
});

test("an IPv6 address that carries an IPv4 address is read as that address, and back", () => {
  for (const [address, ipv4] of [
    ["[::ffff:a9fe:a9fe]", "169.254.169.254"],
    ["::ffff:a9fe:a9fe", "169.254.169.254"],
    ["[0:0:0:0:0:FFFF:A9FE:A9FE]", "169.254.169.254"],
    ["[::a9fe:a9fe]", "169.254.169.254"],
    ["[64:ff9b::a9fe:a9fe]", "169.254.169.254"],
    ["[::ffff:7f00:1]", "127.0.0.1"],
  ] as const)
    assert.equal(carriedIPv4(address), ipv4, address);
  for (const address of ["[::1]", "[::]", "[fd00::1]", "[2001:db8::a9fe:a9fe]", "[::ffff:1:2:3]", "[1::2::3]", "example.com", "10.0.0.2"])
    assert.equal(carriedIPv4(address), undefined, address);
  assert.deepEqual(carryingIPv6("169.254.169.254"), ["[::ffff:a9fe:a9fe]", "[::a9fe:a9fe]", "[64:ff9b::a9fe:a9fe]"]);
  assert.deepEqual(carryingIPv6("0.0.0.1"), ["[::ffff:0:1]", "[64:ff9b::0:1]"]);
  for (const ipv4 of ["169.254.169.254", "127.0.0.1", "10.0.0.2"])
    for (const host of carryingIPv6(ipv4)) {
      assert.equal(new URL(`http://${host}/`).hostname, host, host);
      assert.equal(carriedIPv4(host), ipv4, host);
    }
  for (const value of ["example.com", "1.2.3", "256.1.1.1", "1.2.3.4.5"]) assert.deepEqual(carryingIPv6(value), [], value);
});
