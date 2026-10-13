// Includes MIT-licensed code; see THIRD-PARTY-NOTICES.md.
// URL policy shared by the hosts and the page. Browser-free so it stays unit-testable.

/** Schemes that reach a network site, and the HTTP(S) origin each opens on. */
const NETWORK: Record<string, string> = {
  "http:": "http:",
  "https:": "https:",
  "ws:": "http:",
  "wss:": "https:",
};

/**
 * The origin site rules judge a request by: an HTTP(S) URL's own, a
 * WebSocket's HTTP(S) twin (`wss://a.example/x` → `https://a.example`), a
 * domain without its trailing dot (`example.com.` is the same site as
 * `example.com`), and never credentials. `credentials` says whether the URL
 * carried a user name or password, which request filters refuse rather than
 * judge. None for addresses that reach no site of their own (`data:`,
 * `blob:`, `about:`) or are not URLs.
 */
export function policyOrigin(
  input: string,
): { origin: string; credentials: boolean } | undefined {
  if (!URL.canParse(input)) return;
  const url = new URL(input);
  const protocol = NETWORK[url.protocol];
  if (!protocol) return;
  const credentials = !!(url.username || url.password);
  url.protocol = protocol;
  const host = url.hostname;
  // An address never keeps a trailing dot; URL parsing already drops it.
  if (host.length > 1 && host.endsWith(".")) url.hostname = host.slice(0, -1);
  return { origin: url.origin, credentials };
}

/** The HTTP(S) origin of a credential-free HTTP(S) URL, as site rules read it. */
export function destinationOrigin(input: string) {
  if (!URL.canParse(input) || !/^https?:$/.test(new URL(input).protocol)) return;
  const target = policyOrigin(input);
  return target && !target.credentials ? target.origin : undefined;
}

export function normalizeURL(input: string) {
  const value = input.trim() || "about:blank";
  const local =
    /^(?:localhost|127(?:\.\d{1,3}){3}|\[::1\])(?::\d+)?(?:[/?#]|$)/i.test(
      value,
    );
  const url =
    value === "about:blank" || /^[a-z][a-z\d+.-]*:\/\//i.test(value)
      ? value
      : `${local ? "http" : "https"}://${value}`;
  if (url !== "about:blank" && !destinationOrigin(url))
    throw new Error("Only HTTP, HTTPS, and about:blank URLs are supported.");
  return url;
}

/**
 * The same URL or origin on its `www.` twin host (www.example.com ↔
 * example.com), so one approval covers both spellings of a site. None for
 * non-HTTP(S) URLs, IP addresses, single-label hosts such as localhost, and
 * www.<tld>.
 */
export function wwwTwin(input: string) {
  if (!URL.canParse(input)) return;
  const url = new URL(input);
  const host = url.hostname;
  if (
    !/^https?:$/.test(url.protocol) ||
    host.startsWith("[") ||
    /^\d+(?:\.\d+){3}$/.test(host)
  )
    return;
  const twin = host.startsWith("www.") ? host.slice(4) : `www.${host}`;
  if (!host.includes(".") || !twin.includes(".")) return;
  const origin = input === url.origin;
  url.hostname = twin;
  return origin ? url.origin : url.href;
}

/**
 * The permission resources of loading `input`: its normalised URL, or with
 * `origins` its HTTP(S) origin, plus its `www.` twin with `twin`. Throws
 * `normalizeURL`'s error for other schemes.
 */
export function navigationResources(
  input: string,
  options: { origins?: boolean; twin?: boolean } = {},
) {
  const href = new URL(normalizeURL(input)).href;
  const first = options.origins ? (destinationOrigin(href) ?? href) : href;
  const twin = options.twin ? wwwTwin(first) : undefined;
  return twin ? [first, twin] : [first];
}

export type SiteDecision = "allow" | "ask" | "deny";

/**
 * Picks the rule that applies to an origin. Exact origins take priority;
 * otherwise the most specific subdomain rule (`https://*.example.com`) wins;
 * otherwise the defaults. Works for any set of categories (Desktop's three,
 * Chrome's four with scripts), taken from the defaults. `input` is an origin
 * or URL, read as `policyOrigin` reads it.
 */
export function browserSiteAccess<Category extends string>(
  policy: {
    defaults: Record<Category, SiteDecision>;
    sites: readonly (Record<NoInfer<Category>, SiteDecision> & {
      origin: string;
    })[];
  },
  input: string,
): Record<Category, SiteDecision> {
  // Rules hold canonical origins; read the same site however it is spelled.
  const origin = policyOrigin(input)?.origin ?? input;
  const match = (pattern: string) => {
    if (pattern === origin) return true;
    const index = pattern.indexOf("*.");
    if (index < 0 || !origin.startsWith(pattern.slice(0, index))) return false;
    const host = origin.slice(index), suffix = pattern.slice(index + 2);
    if (!host.endsWith(suffix)) return false;
    const subdomain = host.slice(0, -suffix.length);
    return subdomain.length > 1 && subdomain.endsWith(".");
  };
  return policy.sites.filter(site => match(site.origin)).sort((a,b) =>
    Number(a.origin.includes("*")) - Number(b.origin.includes("*")) || b.origin.length - a.origin.length
  )[0] ?? policy.defaults;
}
