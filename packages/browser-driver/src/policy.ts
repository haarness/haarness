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
  const decide = (origin: string) => {
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
  };
  const access = decide(origin);
  // An IPv6 address that carries an IPv4 one reaches that host: the IPv4
  // address's rule holds too, where it is stricter.
  const ipv4 = carriedIPv4Origin(origin);
  if (!ipv4) return access;
  const other = decide(ipv4);
  const strictness = { allow: 0, ask: 1, deny: 2 } as const;
  const stricter: Partial<Record<Category, SiteDecision>> = {};
  for (const category of Object.keys(policy.defaults) as Category[]) {
    const [mine, theirs] = [access[category], other[category]];
    stricter[category] = strictness[theirs] > strictness[mine] ? theirs : mine;
  }
  return stricter as Record<Category, SiteDecision>;
}

/**
 * The IPv4 address an IPv6 address carries, where a connection to the IPv6
 * address reaches that IPv4 host: IPv4-mapped (`::ffff:169.254.169.254`),
 * IPv4-compatible (`::169.254.169.254`) and the well-known NAT64 prefix
 * (`64:ff9b::169.254.169.254`). `::` and `::1` carry none. `address` is the
 * IPv6 address as a URL's host writes it, inside its brackets or not.
 */
export function carriedIPv4(address: string): string | undefined {
  const text = address.replace(/^\[|\]$/g, "").toLowerCase();
  if (!text.includes(":")) return;
  const halves = text.split("::");
  if (halves.length > 2) return;
  const words = (part: string) => (part ? part.split(":") : []);
  const head = words(halves[0]!), tail = halves.length === 2 ? words(halves[1]!) : [];
  const missing = 8 - head.length - tail.length;
  if (halves.length === 1 ? missing !== 0 : missing < 1) return;
  const groups = [...head, ...Array<string>(missing).fill("0"), ...tail];
  if (!groups.every((group) => /^[0-9a-f]{1,4}$/.test(group))) return;
  const [a, b, c, d, e, f, g, h] = groups.map((group) => parseInt(group, 16)) as [
    number, number, number, number, number, number, number, number,
  ];
  const zero = a === 0 && b === 0 && c === 0 && d === 0 && e === 0;
  const carries =
    (zero && f === 0xffff) || (a === 0x64 && b === 0xff9b && c === 0 && d === 0 && e === 0 && f === 0) || (zero && f === 0 && g !== 0);
  if (!carries) return;
  return [g >> 8, g & 255, h >> 8, h & 255].join(".");
}

/**
 * The IPv6 addresses that carry `ipv4` (see `carriedIPv4`), as a URL writes
 * them: `[::ffff:a9fe:a9fe]`, `[::a9fe:a9fe]` and `[64:ff9b::a9fe:a9fe]` for
 * `169.254.169.254`. None for anything that is not a dotted IPv4 address.
 */
export function carryingIPv6(ipv4: string): string[] {
  const octets = ipv4.split(".").map((octet) => (/^\d{1,3}$/.test(octet) ? Number(octet) : NaN));
  if (octets.length !== 4 || octets.some((octet) => !(octet >= 0 && octet <= 255))) return [];
  const [a, b, c, d] = octets as [number, number, number, number];
  const high = ((a << 8) | b).toString(16), low = ((c << 8) | d).toString(16);
  const words = `${high}:${low}`;
  return [`[::ffff:${words}]`, ...(high === "0" ? [] : [`[::${words}]`]), `[64:ff9b::${words}]`];
}

/** `origin` with its IPv6 host written as the IPv4 address it carries, when it carries one. */
function carriedIPv4Origin(origin: string) {
  if (!URL.canParse(origin)) return;
  const url = new URL(origin);
  const ipv4 = url.hostname.startsWith("[") ? carriedIPv4(url.hostname) : undefined;
  if (!ipv4) return;
  url.hostname = ipv4;
  return url.origin;
}
