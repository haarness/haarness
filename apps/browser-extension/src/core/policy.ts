/**
 * Which pages and sites the agent may touch in Chrome. The URL helpers match
 * Desktop's (`browser/policy.ts`, `browserSiteAccess`) so one rule reads the
 * same in both browsers; they move to the shared driver at integration.
 */
import type { Access, Method, Policy } from "./browser";

export function destinationOrigin(input: string): string | undefined {
  if (!URL.canParse(input)) return;
  const url = new URL(input);
  return /^https?:$/.test(url.protocol) && !url.username && !url.password ? url.origin : undefined;
}

export function normalizeURL(input: string): string {
  const value = input.trim() || "about:blank";
  const local = /^(?:localhost|127(?:\.\d{1,3}){3}|\[::1\])(?::\d+)?(?:[/?#]|$)/i.test(value);
  const url =
    value === "about:blank" || /^[a-z][a-z\d+.-]*:\/\//i.test(value) ? value : `${local ? "http" : "https"}://${value}`;
  if (url !== "about:blank" && !destinationOrigin(url))
    throw new Error("Only HTTP, HTTPS, and about:blank URLs are supported.");
  return url;
}

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
  const match = (pattern: string) => {
    if (pattern === origin) return true;
    const index = pattern.indexOf("*.");
    if (index < 0 || !origin.startsWith(pattern.slice(0, index))) return false;
    const host = origin.slice(index),
      suffix = pattern.slice(index + 2);
    if (!host.endsWith(suffix)) return false;
    const subdomain = host.slice(0, -suffix.length);
    return subdomain.length > 1 && subdomain.endsWith(".");
  };
  return (
    policy.sites
      .filter((site) => match(site.origin))
      .sort(
        (a, b) => Number(a.origin.includes("*")) - Number(b.origin.includes("*")) || b.origin.length - a.origin.length,
      )[0] ?? policy.defaults
  );
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
