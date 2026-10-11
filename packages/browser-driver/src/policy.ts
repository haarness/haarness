// Includes MIT-licensed code; see THIRD-PARTY-NOTICES.md.
// URL policy shared by the hosts and the page. Browser-free so it stays unit-testable.

export function destinationOrigin(input: string) {
  if (!URL.canParse(input)) return;
  const url = new URL(input);
  return /^https?:$/.test(url.protocol) && !url.username && !url.password
    ? url.origin
    : undefined;
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

/**
 * Picks the rule that applies to an origin. Exact origins take priority;
 * otherwise the most specific subdomain rule (`https://*.example.com`) wins;
 * otherwise the defaults. Works for any set of categories (Desktop's three,
 * Chrome's four with scripts).
 */
export function browserSiteAccess<Access extends object>(
  policy: {
    defaults: Access;
    sites: readonly (NoInfer<Access> & { origin: string })[];
  },
  origin: string,
): Access {
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
