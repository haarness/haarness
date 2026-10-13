/**
 * Which pages and sites the agent may touch in Chrome. The URL and site-rule
 * helpers are Desktop's, from the shared driver, so one rule reads the same
 * in both browsers and on the server.
 */
import { browserSiteAccess } from "@haarness/browser-driver/policy";
import type { Access, Method, Policy } from "./browser";

export { destinationOrigin, normalizeURL, policyOrigin } from "@haarness/browser-driver/policy";

/**
 * The same site with `www.` added or removed, so one approval covers both
 * spellings of an address. Hosts that are IP addresses, single labels or
 * `localhost` have no twin.
 */
export function wwwTwin(origin: string): string | undefined {
  if (!URL.canParse(origin)) return;
  const url = new URL(origin);
  const host = url.hostname;
  if (/^[\d.]+$/.test(host) || host.startsWith("[") || !host.includes(".")) return;
  if (host === "localhost" || host.endsWith(".localhost")) return;
  const twin = host.startsWith("www.") ? host.slice(4) : "www." + host;
  if (!twin.includes(".")) return;
  url.hostname = twin;
  return url.origin;
}

/** Exact origins take priority; otherwise the most specific subdomain rule wins. */
export function siteAccess(policy: Policy, origin: string): Access {
  return browserSiteAccess(policy, origin);
}

/**
 * Pages Haarness never controls: Chrome's own pages, extension pages
 * (including this extension's), DevTools, local files, page sources, and
 * anything else that is not a web page. A blank page is fine.
 */
export function restricted(url: string): boolean {
  if (url === "" || url === "about:blank") return false;
  return !/^https?:\/\//i.test(url);
}

/** Pages with an opaque origin only allow operations that cannot use the user's identity. */
const OPAQUE_ALLOWED = new Set<Method>([
  "tabs.list", "tabs.open", "tabs.focus", "tabs.close",
  "navigate", "snapshot", "screenshot", "wait", "frames", "find",
]);
export function allowedOnBlank(method: Method): boolean {
  return OPAQUE_ALLOWED.has(method);
}

/** Tools Chrome's debugger cannot run for a single tab. */
const UNSUPPORTED = /^(?:lighthouse|heap\.|cpu\.|trace\.)/;
export function supportedInChrome(method: Method): boolean {
  return !UNSUPPORTED.test(method);
}
